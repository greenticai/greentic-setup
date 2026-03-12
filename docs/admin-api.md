# Admin API Reference

The greentic-setup Admin API provides mTLS-secured endpoints for runtime bundle lifecycle management. These endpoints are served by greentic-operator on a separate admin port.

## Overview

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/admin/status` | GET | Get bundle deployment status |
| `/admin/deploy` | POST | Deploy or upgrade a bundle |
| `/admin/remove` | POST | Remove bundle components |
| `/admin/qa/spec` | POST | Get QA FormSpec for a provider |
| `/admin/qa/validate` | POST | Validate QA answers |
| `/admin/qa/submit` | POST | Submit and persist QA answers |
| `/admin/card/create` | POST | Create card setup session |
| `/admin/card/spec` | GET | Get card form spec for session |
| `/admin/card/submit` | POST | Submit answers via card session |

---

## Authentication

All admin endpoints require mTLS (mutual TLS) authentication. The server validates:

1. Client certificate signed by trusted CA
2. Client CN (Common Name) against allowed list

```bash
# Example request with mTLS
curl --cert client.crt --key client.key --cacert ca.crt \
  -X GET https://localhost:9443/admin/status
```

See [mTLS Setup Guide](./mtls-setup.md) for certificate configuration.

---

## Endpoints

### GET /admin/status

Get the current bundle deployment status.

**Request:**
```bash
curl --cert client.crt --key client.key --cacert ca.crt \
  -X GET https://localhost:9443/admin/status
```

**Response:**
```json
{
  "success": true,
  "data": {
    "bundle_path": "/path/to/bundle",
    "status": "active",
    "pack_count": 3,
    "tenant_count": 2,
    "provider_count": 3
  }
}
```

**Status values:**
- `active` - Bundle is running normally
- `deploying` - Bundle deployment in progress
- `removing` - Bundle removal in progress
- `error` - Bundle has errors

---

### POST /admin/deploy

Deploy a new bundle or upgrade an existing one.

**Request:**
```bash
curl --cert client.crt --key client.key --cacert ca.crt \
  -X POST https://localhost:9443/admin/deploy \
  -H "Content-Type: application/json" \
  -d '{
    "bundle_path": "/path/to/bundle",
    "bundle_name": "My Bundle",
    "pack_refs": [
      "oci://ghcr.io/greentic/messaging-telegram:latest",
      "/local/path/to/pack.gtpack"
    ],
    "tenants": [
      {
        "tenant": "demo",
        "team": "default",
        "allow_paths": ["flows/*"]
      }
    ],
    "answers": {
      "messaging-telegram": {
        "bot_token": "123456:ABC-DEF",
        "public_base_url": "https://example.com"
      }
    },
    "dry_run": false
  }'
```

**Request fields:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `bundle_path` | string | Yes | Target bundle path on server |
| `bundle_name` | string | No | Display name for the bundle |
| `pack_refs` | string[] | No | Pack references to install (OCI, file path) |
| `tenants` | TenantSelection[] | No | Tenant configurations |
| `answers` | object | No | Pre-collected QA answers by provider |
| `dry_run` | boolean | No | If true, only plan without executing |

**Response:**
```json
{
  "success": true,
  "data": {
    "bundle_path": "/path/to/bundle",
    "status": "active",
    "pack_count": 2,
    "tenant_count": 1,
    "provider_count": 2
  }
}
```

---

### POST /admin/remove

Remove components from a bundle.

**Request:**
```bash
curl --cert client.crt --key client.key --cacert ca.crt \
  -X POST https://localhost:9443/admin/remove \
  -H "Content-Type: application/json" \
  -d '{
    "bundle_path": "/path/to/bundle",
    "packs": [
      {"pack_id": "messaging-telegram"}
    ],
    "providers": ["messaging-telegram"],
    "tenants": [],
    "dry_run": false
  }'
```

**Request fields:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `bundle_path` | string | Yes | Target bundle path |
| `packs` | PackRemoveSelection[] | No | Packs to remove |
| `providers` | string[] | No | Provider IDs to remove |
| `tenants` | TenantSelection[] | No | Tenants/teams to remove |
| `dry_run` | boolean | No | If true, only plan without executing |

**Response:**
```json
{
  "success": true,
  "data": {
    "removed_packs": ["messaging-telegram"],
    "removed_providers": ["messaging-telegram"]
  }
}
```

---

### POST /admin/qa/spec

Get the QA FormSpec for a provider. Used to render setup forms.

**Request:**
```bash
curl --cert client.crt --key client.key --cacert ca.crt \
  -X POST https://localhost:9443/admin/qa/spec \
  -H "Content-Type: application/json" \
  -d '{
    "bundle_path": "/path/to/bundle",
    "provider_id": "messaging-telegram",
    "locale": "en"
  }'
```

**Response:**
```json
{
  "success": true,
  "data": {
    "title": "Telegram Provider Setup",
    "description": "Configure Telegram bot settings",
    "questions": [
      {
        "id": "bot_token",
        "title": "Bot Token",
        "description": "Token from @BotFather",
        "kind": "string",
        "required": true,
        "secret": true
      },
      {
        "id": "public_base_url",
        "title": "Public Base URL",
        "description": "Webhook callback URL",
        "kind": "string",
        "required": true,
        "secret": false,
        "constraint": {
          "regex": "^https://"
        }
      },
      {
        "id": "default_chat_id",
        "title": "Default Chat ID",
        "kind": "string",
        "required": false,
        "secret": false
      }
    ]
  }
}
```

---

### POST /admin/qa/validate

Validate QA answers against a FormSpec without persisting.

**Request:**
```bash
curl --cert client.crt --key client.key --cacert ca.crt \
  -X POST https://localhost:9443/admin/qa/validate \
  -H "Content-Type: application/json" \
  -d '{
    "bundle_path": "/path/to/bundle",
    "provider_id": "messaging-telegram",
    "answers": {
      "bot_token": "123456:ABC-DEF",
      "public_base_url": "https://example.com"
    }
  }'
```

**Response (valid):**
```json
{
  "success": true,
  "data": {
    "valid": true,
    "errors": []
  }
}
```

**Response (invalid):**
```json
{
  "success": false,
  "data": {
    "valid": false,
    "errors": [
      {
        "field": "public_base_url",
        "message": "Must start with https://"
      }
    ]
  }
}
```

---

### POST /admin/qa/submit

Submit and persist QA answers. Optionally triggers hot reload.

**Request:**
```bash
curl --cert client.crt --key client.key --cacert ca.crt \
  -X POST https://localhost:9443/admin/qa/submit \
  -H "Content-Type: application/json" \
  -d '{
    "bundle_path": "/path/to/bundle",
    "provider_id": "messaging-telegram",
    "tenant": "demo",
    "team": "default",
    "answers": {
      "bot_token": "123456:ABC-DEF",
      "public_base_url": "https://example.com"
    },
    "reload": true
  }'
```

**Request fields:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `bundle_path` | string | Yes | Bundle path |
| `provider_id` | string | Yes | Provider to configure |
| `tenant` | string | Yes | Tenant ID |
| `team` | string | No | Team ID |
| `answers` | object | Yes | QA answers to persist |
| `reload` | boolean | No | Trigger hot reload after persist |

**Response:**
```json
{
  "success": true,
  "data": {
    "persisted_keys": ["bot_token", "public_base_url"],
    "reloaded": true
  }
}
```

---

### POST /admin/card/create

Create a new adaptive card setup session.

**Request:**
```bash
curl --cert client.crt --key client.key --cacert ca.crt \
  -X POST https://localhost:9443/admin/card/create \
  -H "Content-Type: application/json" \
  -d '{
    "bundle_path": "/path/to/bundle",
    "provider_id": "messaging-telegram",
    "tenant": "demo",
    "team": "default",
    "ttl_secs": 1800
  }'
```

**Response:**
```json
{
  "success": true,
  "data": {
    "session_id": "setup-1a2b3c4d",
    "expires_at": 1709251200,
    "setup_url": "https://operator.example.com/setup?session=setup-1a2b3c4d&token=xxx&provider=messaging-telegram",
    "card": {
      "type": "AdaptiveCard",
      "version": "1.5",
      "body": [...]
    }
  }
}
```

See [Adaptive Cards Guide](./adaptive-cards.md) for detailed card setup flow.

---

### GET /admin/card/spec

Get the current card form spec for an active session.

**Request:**
```bash
curl --cert client.crt --key client.key --cacert ca.crt \
  -X GET "https://localhost:9443/admin/card/spec?session=setup-1a2b3c4d&token=xxx"
```

**Response:**
```json
{
  "success": true,
  "data": {
    "session_id": "setup-1a2b3c4d",
    "current_step": 0,
    "total_steps": 2,
    "card": {
      "type": "AdaptiveCard",
      "version": "1.5",
      "body": [...]
    }
  }
}
```

---

### POST /admin/card/submit

Submit answers from an adaptive card.

**Request:**
```bash
curl --cert client.crt --key client.key --cacert ca.crt \
  -X POST https://localhost:9443/admin/card/submit \
  -H "Content-Type: application/json" \
  -d '{
    "session_id": "setup-1a2b3c4d",
    "token": "xxx",
    "answers": {
      "bot_token": "123456:ABC-DEF",
      "public_base_url": "https://example.com"
    }
  }'
```

**Response (more steps):**
```json
{
  "success": true,
  "data": {
    "complete": false,
    "next_card": {
      "type": "AdaptiveCard",
      "version": "1.5",
      "body": [...]
    },
    "warnings": []
  }
}
```

**Response (complete):**
```json
{
  "success": true,
  "data": {
    "complete": true,
    "next_card": null,
    "warnings": [],
    "persisted_keys": ["bot_token", "public_base_url"]
  }
}
```

---

## Error Responses

All endpoints return errors in a consistent format:

```json
{
  "success": false,
  "error": "Error message describing what went wrong"
}
```

**Common HTTP status codes:**

| Code | Description |
|------|-------------|
| 200 | Success |
| 400 | Bad request (invalid JSON, missing fields) |
| 401 | Unauthorized (mTLS auth failed) |
| 403 | Forbidden (client CN not allowed) |
| 404 | Not found (bundle/provider not found) |
| 500 | Internal server error |

---

## Type Definitions

### TenantSelection

```json
{
  "tenant": "demo",
  "team": "default",
  "allow_paths": ["flows/*", "packs/*"]
}
```

### PackRemoveSelection

```json
{
  "pack_id": "messaging-telegram",
  "remove_flows": true,
  "remove_secrets": false
}
```

### BundleStatus

Enum values: `"active"`, `"deploying"`, `"removing"`, `"error"`

---

## Configuration

### Operator Admin Port

```bash
# Start operator with admin API on port 9443
gtc op demo start --bundle ./my-bundle --admin-port 9443
```

### Admin TLS Config

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
```

---

## See Also

- [mTLS Setup Guide](./mtls-setup.md) - Certificate generation
- [Adaptive Cards Guide](./adaptive-cards.md) - Card-based setup flow
- [Demo Guide](./demo-guide.md) - Getting started with bundles
