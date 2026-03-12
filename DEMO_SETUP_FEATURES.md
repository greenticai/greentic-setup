# Demo: greentic-setup Advanced Features

This demo showcases the greentic-setup capabilities:
- Complex QA with conditional questions (`visible_if`)
- Secret masking and secure persistence
- Bundle lifecycle: add/setup/update/remove
- CLI and Admin API endpoints

---

## Prerequisites

```bash
# Install tools
cargo install --path ../greentic
cargo install --path .

# Verify
gtc doctor
```

---

## Part 1: Bundle Lifecycle Management

### 1.1 Initialize Bundle

```bash
# Create new bundle
gtc setup bundle init ./demo-bundle --name "Setup Demo"

# Check structure
ls -la ./demo-bundle/
```

**Expected output:**
```
demo-bundle/
├── greentic.bundle.yaml    # Bundle manifest
├── packs/                  # Pack storage
├── secrets/                # Dev secrets store
└── config/                 # Configuration
```

### 1.2 Add Pack to Bundle

```bash
# Add messaging pack (Telegram example)
gtc setup bundle add ../greentic-messaging-providers/packs/messaging-dummy/dist/messaging-dummy.gtpac \
  --bundle ./demo-bundle

# List installed packs
gtc setup bundle list --bundle ./demo-bundle
```

### 1.3 Check Bundle Status

```bash
gtc setup bundle status --bundle ./demo-bundle
gtc setup bundle status --bundle ./demo-bundle --format json
```

---

## Part 2: Complex QA with Conditional Questions

### 2.1 Generate Answers Template

```bash
# Dry run to see questions without executing
gtc setup --dry-run ./demo-bundle

# Export answers template
gtc setup --dry-run --emit-answers ./answers-template.json ./demo-bundle
```

### 2.2 Example: Conditional Questions (visible_if)

The QA system supports conditional visibility:

```yaml
# Example provider QA spec
questions:
  - id: auth_method
    prompt: "Authentication method"
    type: select
    options: ["api_key", "oauth", "none"]

  - id: api_key
    prompt: "Enter API Key"
    type: secret
    visible_if: "auth_method == 'api_key'"   # Only shown if api_key selected

  - id: oauth_client_id
    prompt: "OAuth Client ID"
    type: string
    visible_if: "auth_method == 'oauth'"     # Only shown if oauth selected

  - id: oauth_client_secret
    prompt: "OAuth Client Secret"
    type: secret
    visible_if: "auth_method == 'oauth'"
```

### 2.3 Interactive Wizard with Conditions

```bash
# Run interactive wizard - questions appear based on previous answers
gtc setup ./demo-bundle
```

**Demo flow:**
1. Wizard asks "Authentication method" → select "oauth"
2. Wizard shows OAuth fields (api_key field is hidden)
3. Secrets are masked in terminal output

### 2.4 Answers File with Conditions

```json
{
  "messaging-telegram": {
    "auth_method": "api_key",
    "api_key": "bot123456:ABC-DEF...",
    "webhook_base_url": "https://example.ngrok.io"
  }
}
```

```bash
# Apply answers (skips invisible questions automatically)
gtc setup --answers ./answers.json ./demo-bundle
```

---

## Part 3: Secret Masking & Secure Persistence

### 3.1 Secret Types

Questions with `type: secret` are:
- Masked in terminal (shows `********`)
- Stored in secrets backend (not plain config)
- Never logged or echoed

```bash
# Secrets are stored separately from config
ls ./demo-bundle/secrets/dev/
```

### 3.2 Secrets Backend Options

```yaml
# greentic.bundle.yaml
secrets:
  backend: dev-store          # Local file (dev only)
  # backend: aws-secrets      # AWS Secrets Manager
  # backend: azure-keyvault   # Azure Key Vault
  # backend: hashicorp-vault  # HashiCorp Vault
```

---

## Part 4: Update & Remove Providers

### 4.1 Update Provider Configuration

```bash
# Update existing provider setup
gtc setup bundle update messaging-telegram \
  --bundle ./demo-bundle \
  --answers ./updated-answers.json

# Or interactive update
gtc setup bundle update messaging-telegram --bundle ./demo-bundle
```

### 4.2 Remove Provider

```bash
# Remove provider (requires --force for confirmation)
gtc setup bundle remove messaging-telegram \
  --bundle ./demo-bundle \
  --force

# Verify removal
gtc setup bundle status --bundle ./demo-bundle
```

---

## Part 5: Admin API (mTLS)

### 5.1 Admin Endpoint Types

greentic-setup exports types for admin API integration:

```rust
// Request types
pub struct SetupRequest {
    pub provider_id: String,
    pub answers: HashMap<String, Value>,
}

pub struct UpdateRequest {
    pub provider_id: String,
    pub answers: HashMap<String, Value>,
}

pub struct RemoveRequest {
    pub provider_id: String,
    pub force: bool,
}

// Response types
pub struct SetupResponse {
    pub success: bool,
    pub provider_id: String,
    pub message: Option<String>,
}
```

### 5.2 Admin TLS Configuration

```rust
pub struct AdminTlsConfig {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
    pub ca_path: Option<PathBuf>,  // For mTLS client verification
}
```

### 5.3 Using Admin API via Operator

```bash
# Operator exposes admin endpoint on separate port
gtc op demo start --bundle ./demo-bundle --admin-port 9443

# Example curl with mTLS
curl --cert client.crt --key client.key --cacert ca.crt \
  -X POST https://localhost:9443/admin/setup \
  -H "Content-Type: application/json" \
  -d '{"provider_id": "messaging-telegram", "answers": {...}}'
```

---

## Part 6: Hot Reload (Diff-Based)

### 6.1 Bundle Diff

greentic-setup computes diffs between bundle states:

```rust
pub struct BundleDiff {
    pub added: Vec<ProviderInfo>,
    pub removed: Vec<ProviderInfo>,
    pub updated: Vec<ProviderDiff>,
}
```

### 6.2 Reload Plan

```bash
# Preview what would change
gtc setup bundle diff ./demo-bundle --from-snapshot ./previous-state.json

# Apply with hot reload (no restart needed for config changes)
gtc setup bundle apply --hot-reload ./demo-bundle
```

---

## Part 7: Adaptive Cards Setup (Nice to Have)

### 7.1 Card Setup Session

```rust
pub struct CardSetupSession {
    pub session_id: String,
    pub provider_id: String,
    pub current_step: usize,
    pub answers: HashMap<String, Value>,
    pub card: AdaptiveCard,  // Current card to render
}
```

### 7.2 Security Considerations

For secure adaptive card setup:

1. **Session-based**: Each setup gets unique session ID
2. **Token validation**: Cards include signed tokens
3. **mTLS for submission**: Card responses go through admin endpoint
4. **Timeout**: Sessions expire after inactivity

```bash
# Start card-based setup session
curl -X POST https://localhost:9443/admin/setup/card-session \
  -d '{"provider_id": "messaging-telegram"}'

# Returns adaptive card JSON for rendering in Teams/WebChat
```

---

## Quick Demo Script

```bash
#!/bin/bash
set -e

echo "=== greentic-setup Demo ==="

# 1. Init bundle
rm -rf /tmp/setup-demo
gtc setup bundle init /tmp/setup-demo --name "Demo"

# 2. Add pack
gtc setup bundle add ../greentic-messaging-providers/dist/messaging-telegram.gtpack \
  --bundle /tmp/setup-demo

# 3. Generate answers template
gtc setup --dry-run --emit-answers /tmp/answers.json /tmp/setup-demo
echo "Generated answers template:"
cat /tmp/answers.json

# 4. Fill answers (would normally edit file)
# For demo, create minimal answers
cat > /tmp/answers.json << 'EOF'
{
  "messaging-telegram": {
    "bot_token": "demo:token",
    "webhook_url": "https://demo.example.com/webhook"
  }
}
EOF

# 5. Apply setup
gtc setup --answers /tmp/answers.json /tmp/setup-demo

# 6. Check status
gtc setup bundle status --bundle /tmp/setup-demo

# 7. Update (change webhook)
cat > /tmp/updated-answers.json << 'EOF'
{
  "messaging-telegram": {
    "webhook_url": "https://new-webhook.example.com/webhook"
  }
}
EOF
gtc setup bundle update messaging-telegram \
  --bundle /tmp/setup-demo \
  --answers /tmp/updated-answers.json

# 8. Remove
gtc setup bundle remove messaging-telegram \
  --bundle /tmp/setup-demo \
  --force

echo "=== Demo Complete ==="
```

---

## Implementation Status

| Feature | Status |
|---------|--------|
| Bundle init/add/list/status | ✅ Done |
| Interactive wizard | ✅ Done |
| Conditional questions (visible_if) | ✅ Done |
| Secret masking | ✅ Done |
| Visibility-aware persistence | ✅ Done |
| Bundle update | ✅ Done |
| Bundle remove | ✅ Done |
| Admin API types | ✅ Done |
| Hot reload diff | ✅ Done |
| Adaptive card session | ✅ Done (types) |
| i18n (66 languages) | ✅ Done |
| gtc passthrough | ✅ Done |

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      User Interface                          │
├─────────────┬─────────────┬─────────────┬──────────────────┤
│   CLI       │  Admin API  │  Adaptive   │  Operator        │
│ (gtc setup) │  (mTLS)     │  Cards      │  Integration     │
└──────┬──────┴──────┬──────┴──────┬──────┴────────┬─────────┘
       │             │             │               │
       └─────────────┴─────────────┴───────────────┘
                            │
                    ┌───────▼───────┐
                    │ greentic-setup │
                    │    Engine      │
                    └───────┬───────┘
                            │
       ┌────────────────────┼────────────────────┐
       │                    │                    │
┌──────▼──────┐     ┌───────▼───────┐    ┌──────▼──────┐
│  Discovery  │     │   QA Bridge   │    │  Secrets    │
│  (.gtpack)  │     │  (FormSpec)   │    │  Backend    │
└─────────────┘     └───────────────┘    └─────────────┘
```
