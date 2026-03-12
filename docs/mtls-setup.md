# mTLS Setup Guide

This guide explains how to set up mutual TLS (mTLS) for the greentic-setup Admin API. mTLS ensures both the server and client authenticate each other using certificates.

## Overview

```
┌─────────────────┐                    ┌─────────────────┐
│     Client      │◀──── mTLS ────────▶│     Server      │
│  (Admin Tool)   │                    │   (Operator)    │
└─────────────────┘                    └─────────────────┘
        │                                      │
        │ client.crt ◀───────────────────────▶ │ server.crt
        │ client.key                           │ server.key
        │                                      │
        └───────────── Signed by ─────────────▶│
                         CA                    │
                     (ca.crt)                  │
```

**mTLS provides:**
- Server authentication (client verifies server identity)
- Client authentication (server verifies client identity)
- Encrypted communication (TLS 1.3)
- Access control via CN (Common Name) allowlist

---

## Quick Start

### Generate All Certificates

```bash
#!/bin/bash
# generate-certs.sh - Generate CA, server, and client certificates

CERT_DIR="./certs"
CA_DAYS=3650      # 10 years
CERT_DAYS=365     # 1 year
SERVER_CN="greentic-operator"
CLIENT_CN="greentic-admin"

mkdir -p "$CERT_DIR"
cd "$CERT_DIR"

# 1. Generate CA
openssl genrsa -out ca.key 4096
openssl req -new -x509 -days $CA_DAYS -key ca.key -out ca.crt \
  -subj "/CN=Greentic CA/O=Greentic/C=US"

# 2. Generate Server Certificate
openssl genrsa -out server.key 2048
openssl req -new -key server.key -out server.csr \
  -subj "/CN=$SERVER_CN/O=Greentic/C=US"
openssl x509 -req -days $CERT_DAYS -in server.csr \
  -CA ca.crt -CAkey ca.key -CAcreateserial \
  -out server.crt

# 3. Generate Client Certificate
openssl genrsa -out client.key 2048
openssl req -new -key client.key -out client.csr \
  -subj "/CN=$CLIENT_CN/O=Greentic/C=US"
openssl x509 -req -days $CERT_DAYS -in client.csr \
  -CA ca.crt -CAkey ca.key -CAcreateserial \
  -out client.crt

# Cleanup CSRs
rm -f *.csr

echo "Certificates generated in $CERT_DIR/"
ls -la
```

### Verify Certificates

```bash
# View CA certificate
openssl x509 -in certs/ca.crt -text -noout | head -20

# View server certificate
openssl x509 -in certs/server.crt -text -noout | head -20

# Verify server cert is signed by CA
openssl verify -CAfile certs/ca.crt certs/server.crt

# Verify client cert is signed by CA
openssl verify -CAfile certs/ca.crt certs/client.crt
```

---

## Certificate Types

### 1. CA Certificate (ca.crt, ca.key)

The Certificate Authority signs all other certificates.

```bash
# Generate CA key (4096-bit RSA)
openssl genrsa -out ca.key 4096

# Generate self-signed CA certificate
openssl req -new -x509 -days 3650 -key ca.key -out ca.crt \
  -subj "/CN=Greentic CA/O=Greentic/C=US"
```

**Security:**
- Keep `ca.key` secure and offline
- Only use `ca.crt` on servers and clients
- Use a strong key size (4096-bit recommended)

### 2. Server Certificate (server.crt, server.key)

Used by greentic-operator to identify itself.

```bash
# Generate server key (2048-bit RSA)
openssl genrsa -out server.key 2048

# Generate CSR with SAN (Subject Alternative Names)
cat > server.cnf << EOF
[req]
default_bits = 2048
prompt = no
distinguished_name = dn
req_extensions = req_ext

[dn]
CN = greentic-operator
O = Greentic
C = US

[req_ext]
subjectAltName = @alt_names

[alt_names]
DNS.1 = localhost
DNS.2 = operator.local
DNS.3 = *.greentic.io
IP.1 = 127.0.0.1
IP.2 = 10.0.0.1
EOF

openssl req -new -key server.key -out server.csr -config server.cnf

# Sign with CA
openssl x509 -req -days 365 -in server.csr \
  -CA ca.crt -CAkey ca.key -CAcreateserial \
  -out server.crt \
  -extensions req_ext -extfile server.cnf
```

**Note:** Include all hostnames/IPs the server will use in SAN.

### 3. Client Certificate (client.crt, client.key)

Used by admin tools to authenticate to the server.

```bash
# Generate client key
openssl genrsa -out client.key 2048

# Generate CSR with specific CN
openssl req -new -key client.key -out client.csr \
  -subj "/CN=greentic-admin/O=Greentic/C=US"

# Sign with CA
openssl x509 -req -days 365 -in client.csr \
  -CA ca.crt -CAkey ca.key -CAcreateserial \
  -out client.crt
```

**The CN is used for access control** (see [Access Control](#access-control)).

---

## Configuration

### Operator Configuration

```yaml
# greentic.operator.yaml
admin:
  port: 9443
  tls:
    server_cert: /etc/greentic/admin/server.crt
    server_key: /etc/greentic/admin/server.key
    client_ca: /etc/greentic/admin/ca.crt
    allowed_clients:
      - "CN=greentic-admin"
      - "CN=deploy-bot"
      - "CN=ci-pipeline"
```

### AdminTlsConfig (Rust)

```rust
use greentic_setup::admin::AdminTlsConfig;

let config = AdminTlsConfig {
    server_cert: "/etc/greentic/admin/server.crt".into(),
    server_key: "/etc/greentic/admin/server.key".into(),
    client_ca: "/etc/greentic/admin/ca.crt".into(),
    allowed_clients: vec![
        "CN=greentic-admin".to_string(),
        "CN=deploy-bot".to_string(),
    ],
    port: 9443,
};

// Validate all cert files exist
config.validate()?;
```

---

## Access Control

### CN-Based Allowlist

The server checks the client certificate's CN against `allowed_clients`:

```yaml
allowed_clients:
  - "CN=greentic-admin"     # Exact match
  - "CN=deploy-*"           # Wildcard (if implemented)
  - "*"                     # Allow any valid client cert
```

### Implementation

```rust
impl AdminTlsConfig {
    pub fn is_client_allowed(&self, cn: &str) -> bool {
        if self.allowed_clients.is_empty() {
            return true;  // No restrictions
        }
        self.allowed_clients
            .iter()
            .any(|pattern| pattern == cn || pattern == "*")
    }
}
```

### Extracting CN from Client Cert

The TLS terminator (nginx, envoy, or built-in) extracts the CN and passes it as a header:

```
X-Client-CN: greentic-admin
```

The admin handler validates this:

```rust
fn check_mtls_auth(req: &Request, config: &AdminTlsConfig) -> Result<(), Response> {
    let cn = req.headers()
        .get("X-Client-CN")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| unauthorized("Missing client certificate"))?;

    if !config.is_client_allowed(cn) {
        return Err(forbidden(&format!("Client {} not allowed", cn)));
    }

    Ok(())
}
```

---

## Usage Examples

### curl

```bash
# Basic request with mTLS
curl --cert client.crt --key client.key --cacert ca.crt \
  -X GET https://localhost:9443/admin/status

# With verbose TLS debugging
curl -v --cert client.crt --key client.key --cacert ca.crt \
  -X GET https://localhost:9443/admin/status

# Using PEM bundle (cert + key in one file)
cat client.crt client.key > client.pem
curl --cert client.pem --cacert ca.crt \
  -X GET https://localhost:9443/admin/status
```

### Python (requests)

```python
import requests

response = requests.get(
    "https://localhost:9443/admin/status",
    cert=("client.crt", "client.key"),
    verify="ca.crt"
)
print(response.json())
```

### Node.js (axios)

```javascript
const https = require('https');
const fs = require('fs');
const axios = require('axios');

const httpsAgent = new https.Agent({
  cert: fs.readFileSync('client.crt'),
  key: fs.readFileSync('client.key'),
  ca: fs.readFileSync('ca.crt'),
});

axios.get('https://localhost:9443/admin/status', { httpsAgent })
  .then(res => console.log(res.data));
```

### Rust (reqwest)

```rust
use reqwest::Client;
use std::fs;

let cert = fs::read("client.crt")?;
let key = fs::read("client.key")?;
let identity = reqwest::Identity::from_pem(&[cert, key].concat())?;

let ca = fs::read("ca.crt")?;
let ca_cert = reqwest::Certificate::from_pem(&ca)?;

let client = Client::builder()
    .identity(identity)
    .add_root_certificate(ca_cert)
    .build()?;

let resp = client
    .get("https://localhost:9443/admin/status")
    .send()
    .await?;
```

---

## Certificate Rotation

### 1. Generate New Certificates

```bash
# Generate new client cert with same CN
openssl genrsa -out client-new.key 2048
openssl req -new -key client-new.key -out client-new.csr \
  -subj "/CN=greentic-admin/O=Greentic/C=US"
openssl x509 -req -days 365 -in client-new.csr \
  -CA ca.crt -CAkey ca.key -CAcreateserial \
  -out client-new.crt
```

### 2. Deploy New Certificates

```bash
# Replace old certs
mv client-new.crt client.crt
mv client-new.key client.key
```

### 3. Verify

```bash
# Test with new certs
curl --cert client.crt --key client.key --cacert ca.crt \
  -X GET https://localhost:9443/admin/status
```

### Automated Rotation

For production, use a certificate manager like:
- **cert-manager** (Kubernetes)
- **Vault PKI** (HashiCorp)
- **AWS Private CA**

---

## Troubleshooting

### Certificate Verification Failed

```
curl: (60) SSL certificate problem: unable to get local issuer certificate
```

**Solution:** Ensure `--cacert ca.crt` points to the CA that signed the server cert.

### Client Certificate Required

```
curl: (56) OpenSSL SSL_read: error:14094412:SSL routines:ssl3_read_bytes:sslv3 alert bad certificate
```

**Solution:** Provide client cert with `--cert client.crt --key client.key`.

### CN Not Allowed

```json
{"success": false, "error": "Client CN=unknown not in allowed list"}
```

**Solution:** Add the client's CN to `allowed_clients` in config.

### Certificate Expired

```
curl: (60) SSL certificate has expired
```

**Solution:** Generate new certificates (see [Certificate Rotation](#certificate-rotation)).

### View Certificate Details

```bash
# Check expiration
openssl x509 -in client.crt -noout -dates

# Check CN and SAN
openssl x509 -in server.crt -noout -text | grep -A1 "Subject:"

# Check if cert matches key
openssl x509 -in client.crt -noout -modulus | openssl md5
openssl rsa -in client.key -noout -modulus | openssl md5
# Both should match
```

---

## Production Checklist

- [ ] CA key stored securely (HSM or offline)
- [ ] Server cert includes all hostnames in SAN
- [ ] Client certs have unique CNs per service/user
- [ ] `allowed_clients` restricts access appropriately
- [ ] Certificate expiration monitoring configured
- [ ] Rotation procedure documented and tested
- [ ] TLS 1.3 enforced (disable older versions)
- [ ] Strong cipher suites only

---

## Security Best Practices

### 1. Use Strong Keys

```bash
# RSA 4096 for CA
openssl genrsa -out ca.key 4096

# RSA 2048 minimum for server/client
openssl genrsa -out server.key 2048

# Or use ECDSA (faster, smaller keys)
openssl ecparam -genkey -name prime256v1 -out server.key
```

### 2. Short-Lived Certificates

```bash
# 90 days for server/client certs
openssl x509 -req -days 90 ...
```

### 3. Separate CAs for Different Environments

```
production-ca.crt  → Production clients only
staging-ca.crt     → Staging clients only
dev-ca.crt         → Development clients only
```

### 4. Revocation (CRL/OCSP)

For enterprise deployments, implement certificate revocation:

```bash
# Generate CRL
openssl ca -gencrl -out crl.pem -config ca.cnf

# Configure server to check CRL
tls:
  crl: /etc/greentic/admin/crl.pem
```

---

## See Also

- [Admin API Reference](./admin-api.md) - API endpoint documentation
- [Adaptive Cards Guide](./adaptive-cards.md) - Card-based setup flow
- [OpenSSL Documentation](https://www.openssl.org/docs/)
