# Manual Testing Guide

Panduan step-by-step untuk testing semua fitur greentic-setup.

## Prerequisites

```bash
# 1. Install greentic-setup
cd /path/to/greentic-setup
cargo install --path .

# 2. Copy binary ke PATH
cp target/release/greentic-setup ~/.local/bin/

# 3. Verify installation
greentic-setup --version
gtc setup --help
```

---

## Test 1: Bundle Lifecycle (init/add/list/status)

### 1.1 Initialize Bundle

```bash
# Cleanup
rm -rf /tmp/test-bundle

# Initialize
gtc setup bundle init /tmp/test-bundle --name "Test Bundle"
```

**Expected output:**
```
Creating bundle at /tmp/test-bundle...
Bundle created at /tmp/test-bundle

Next steps:
  1. greentic-setup bundle add <pack.gtpack> --bundle /tmp/test-bundle
  2. greentic-setup bundle setup --bundle /tmp/test-bundle --answers answers.yaml
```

**Verify:**
```bash
ls -la /tmp/test-bundle/
# Should see: greentic.bundle.yaml, providers/, config/, etc.
```

### 1.2 Add Pack to Bundle

```bash
# Build a test pack first (if not exists)
cd ../greentic-messaging-providers/packs/messaging-dummy
greentic-pack build --in . --gtpack-out ./dist/messaging-dummy.gtpack

# Add to bundle
gtc setup bundle add \
  ../greentic-messaging-providers/packs/messaging-dummy/dist/messaging-dummy.gtpack \
  --bundle /tmp/test-bundle
```

**Expected output:**
```
Adding pack to bundle...
  Pack ref: .../messaging-dummy.gtpack
  ...
Pack added to bundle successfully.
  Resolved packs: 1
```

### 1.3 List Packs

```bash
gtc setup bundle list --bundle /tmp/test-bundle
```

**Expected output:**
```
Bundle: /tmp/test-bundle
Domain: messaging
Packs found: 1
  - messaging-dummy (messaging)
```

### 1.4 Check Status

```bash
gtc setup bundle status --bundle /tmp/test-bundle
gtc setup bundle status --bundle /tmp/test-bundle --format json
```

**Expected output:**
```
Bundle: /tmp/test-bundle
Valid: yes
Packs: 1 installed
  - providers/messaging/messaging-dummy
Tenants: 2
  - demo
  - default
```

---

## Test 2: Interactive Wizard

### 2.1 Setup Pack with Questions

First, create a pack with QA questions:

```bash
# Modify messaging-dummy to have questions
cat > ../greentic-messaging-providers/packs/messaging-dummy/setup.yaml << 'EOF'
provider_id: dummy
version: 1
title: Dummy Provider Setup
questions:
  - name: api_key
    title: API Key
    kind: string
    required: true
    secret: true
    help: "Enter your API key"
  - name: webhook_url
    title: Webhook URL
    kind: string
    required: true
    help: "Public URL for callbacks"
EOF

# Rebuild pack
cd ../greentic-messaging-providers/packs/messaging-dummy
greentic-pack build --in . --gtpack-out ./dist/messaging-dummy.gtpack

# Re-add to bundle
cd /path/to/greentic-setup
rm -rf /tmp/test-bundle
gtc setup bundle init /tmp/test-bundle --name "Test Bundle"
gtc setup bundle add \
  ../greentic-messaging-providers/packs/messaging-dummy/dist/messaging-dummy.gtpack \
  --bundle /tmp/test-bundle
```

### 2.2 Run Interactive Wizard

```bash
gtc setup /tmp/test-bundle
```

**Expected behavior:**
1. Shows "Found 1 provider(s) to configure"
2. Prompts for "API Key" (masked input)
3. Prompts for "Webhook URL"
4. Completes setup

### 2.3 Dry Run

```bash
gtc setup --dry-run /tmp/test-bundle
```

**Expected output:**
```
wizard plan: mode=create dry_run=true
...
[dry-run] Would setup bundle: /tmp/test-bundle
```

---

## Test 3: Answers File (Non-Interactive)

### 3.1 Generate Answers Template

```bash
gtc setup --dry-run --emit-answers /tmp/answers.json /tmp/test-bundle
cat /tmp/answers.json
```

**Expected output:**
```json
{
  "bundle_source": "/tmp/test-bundle",
  "env": "dev",
  "greentic_setup_version": "1.0.0",
  "setup_answers": {
    "messaging-dummy": {
      "api_key": "",
      "webhook_url": ""
    }
  },
  "team": null,
  "tenant": "demo"
}
```

### 3.2 Fill and Apply Answers

```bash
cat > /tmp/answers.json << 'EOF'
{
  "bundle_source": "/tmp/test-bundle",
  "env": "dev",
  "greentic_setup_version": "1.0.0",
  "setup_answers": {
    "messaging-dummy": {
      "api_key": "sk-test-12345",
      "webhook_url": "https://example.com/webhook"
    }
  },
  "team": null,
  "tenant": "demo"
}
EOF

gtc setup --answers /tmp/answers.json /tmp/test-bundle
```

**Expected output:**
```
Loaded answers from /tmp/answers.json
...
Setup complete: /tmp/test-bundle
```

---

## Test 4: Conditional Questions (visible_if)

### 4.1 Create Pack with Conditional Questions

```bash
cat > ../greentic-messaging-providers/packs/messaging-dummy/setup.yaml << 'EOF'
provider_id: dummy
version: 1
title: Conditional Setup Demo
questions:
  - name: auth_method
    title: Authentication Method
    kind: string
    required: true
    help: "Choose: api_key, oauth, or none"

  - name: api_key
    title: API Key
    kind: string
    required: true
    secret: true
    visible_if:
      field: auth_method
      eq: "api_key"

  - name: oauth_client_id
    title: OAuth Client ID
    kind: string
    required: true
    visible_if:
      field: auth_method
      eq: "oauth"

  - name: oauth_client_secret
    title: OAuth Client Secret
    kind: string
    required: true
    secret: true
    visible_if:
      field: auth_method
      eq: "oauth"

  - name: webhook_url
    title: Webhook URL
    kind: string
    required: true
EOF

# Rebuild and re-add
cd ../greentic-messaging-providers/packs/messaging-dummy
greentic-pack build --in . --gtpack-out ./dist/messaging-dummy.gtpack

cd /path/to/greentic-setup
rm -rf /tmp/test-bundle
gtc setup bundle init /tmp/test-bundle --name "Conditional Test"
gtc setup bundle add \
  ../greentic-messaging-providers/packs/messaging-dummy/dist/messaging-dummy.gtpack \
  --bundle /tmp/test-bundle
```

### 4.2 Test with auth_method = "api_key"

```bash
cat > /tmp/answers.json << 'EOF'
{
  "bundle_source": "/tmp/test-bundle",
  "env": "dev",
  "greentic_setup_version": "1.0.0",
  "setup_answers": {
    "messaging-dummy": {
      "auth_method": "api_key",
      "api_key": "sk-secret-12345",
      "webhook_url": "https://example.com/webhook"
    }
  },
  "team": null,
  "tenant": "demo"
}
EOF

gtc setup --answers /tmp/answers.json /tmp/test-bundle
```

**Expected:** Success (oauth fields not required)

### 4.3 Test with auth_method = "oauth"

```bash
cat > /tmp/answers.json << 'EOF'
{
  "bundle_source": "/tmp/test-bundle",
  "env": "dev",
  "greentic_setup_version": "1.0.0",
  "setup_answers": {
    "messaging-dummy": {
      "auth_method": "oauth",
      "oauth_client_id": "client-123",
      "oauth_client_secret": "secret-456",
      "webhook_url": "https://example.com/webhook"
    }
  },
  "team": null,
  "tenant": "demo"
}
EOF

gtc setup --answers /tmp/answers.json /tmp/test-bundle
```

**Expected:** Success (api_key not required)

### 4.4 Test Missing Required Field (Should Fail)

```bash
cat > /tmp/answers.json << 'EOF'
{
  "bundle_source": "/tmp/test-bundle",
  "env": "dev",
  "greentic_setup_version": "1.0.0",
  "setup_answers": {
    "messaging-dummy": {
      "auth_method": "api_key",
      "webhook_url": "https://example.com/webhook"
    }
  },
  "team": null,
  "tenant": "demo"
}
EOF

gtc setup --answers /tmp/answers.json /tmp/test-bundle
```

**Expected:** Should fail - api_key is required when auth_method = "api_key"

---

## Test 5: Locale / i18n

### 5.1 Test Japanese Locale

```bash
gtc setup bundle status --bundle /tmp/test-bundle --locale ja
```

**Expected output:**
```
Bundle: /tmp/test-bundle
Valid: はい
Pack: 1 インストール済み
...
```

### 5.2 Test Indonesian Locale

```bash
gtc setup bundle status --bundle /tmp/test-bundle --locale id
```

**Expected output:**
```
Bundle: /tmp/test-bundle
Valid: ya
Pack: 1 terinstal
...
```

### 5.3 Test List with Locale

```bash
gtc setup bundle list --bundle /tmp/test-bundle --locale ja
```

**Expected output:**
```
Bundle: /tmp/test-bundle
Domain: messaging
見つかったpack: 1
  - messaging-dummy (messaging)
```

---

## Test 6: Update Provider

### 6.1 Update Configuration

```bash
cat > /tmp/update-answers.json << 'EOF'
{
  "messaging-dummy": {
    "webhook_url": "https://new-webhook.example.com/callback"
  }
}
EOF

gtc setup bundle update messaging-dummy \
  --bundle /tmp/test-bundle \
  --answers /tmp/update-answers.json
```

**Expected:** Configuration updated successfully

---

## Test 7: Remove Provider

### 7.1 Remove with Force

```bash
gtc setup bundle remove messaging-dummy \
  --bundle /tmp/test-bundle \
  --force
```

**Expected output:**
```
Removing provider: messaging-dummy
Provider removed successfully.
```

### 7.2 Verify Removal

```bash
gtc setup bundle status --bundle /tmp/test-bundle
```

**Expected:** Packs: 0 installed

---

## Test 8: Admin API Types (Code Test)

### 8.1 Run Unit Tests

```bash
cargo test admin -- --nocapture
```

**Expected:** All admin tests pass

### 8.2 Test TLS Config Validation

```bash
cargo test tls -- --nocapture
```

**Expected:** TLS config tests pass

---

## Test 9: Card Setup Session (Code Test)

### 9.1 Run Card Setup Tests

```bash
cargo test card_setup -- --nocapture
```

**Expected output:**
```
test card_setup::tests::session_not_expired_within_ttl ... ok
test card_setup::tests::session_expired_with_zero_ttl ... ok
test card_setup::tests::merge_answers_accumulates ... ok
test card_setup::tests::setup_link_generation_signed ... ok
...
```

---

## Test 10: Hot Reload Diff (Code Test)

### 10.1 Run Reload Tests

```bash
cargo test reload -- --nocapture
```

**Expected:** All reload/diff tests pass

---

## Test 11: Full CI Check

```bash
bash ci/local_check.sh
```

**Expected:** All steps pass:
- fmt check
- clippy
- test (97+ tests)
- build
- doc
- package

---

## Test Checklist

| # | Test | Command | Expected |
|---|------|---------|----------|
| 1.1 | Bundle init | `gtc setup bundle init` | Creates bundle dir |
| 1.2 | Bundle add | `gtc setup bundle add` | Resolves packs: 1 |
| 1.3 | Bundle list | `gtc setup bundle list` | Shows 1 pack |
| 1.4 | Bundle status | `gtc setup bundle status` | Valid: yes |
| 2.1 | Interactive wizard | `gtc setup /tmp/test-bundle` | Prompts for input |
| 2.2 | Dry run | `gtc setup --dry-run` | No execution |
| 3.1 | Emit answers | `--emit-answers` | JSON template |
| 3.2 | Apply answers | `--answers` | Setup complete |
| 4.1 | visible_if api_key | auth_method=api_key | api_key required |
| 4.2 | visible_if oauth | auth_method=oauth | oauth fields required |
| 5.1 | Locale ja | `--locale ja` | Japanese output |
| 5.2 | Locale id | `--locale id` | Indonesian output |
| 6.1 | Update provider | `bundle update` | Config updated |
| 7.1 | Remove provider | `bundle remove --force` | Provider removed |
| 8.1 | Admin tests | `cargo test admin` | All pass |
| 9.1 | Card tests | `cargo test card_setup` | All pass |
| 10.1 | Reload tests | `cargo test reload` | All pass |
| 11 | Full CI | `ci/local_check.sh` | All pass |

---

## Cleanup

```bash
# Restore original setup.yaml
cat > ../greentic-messaging-providers/packs/messaging-dummy/setup.yaml << 'EOF'
provider_id: dummy
version: 1
title: Dummy provider setup
questions: []
EOF

# Rebuild
cd ../greentic-messaging-providers/packs/messaging-dummy
greentic-pack build --in . --gtpack-out ./dist/messaging-dummy.gtpack

# Remove test bundle
rm -rf /tmp/test-bundle
rm -f /tmp/answers.json /tmp/update-answers.json
```

---

## Troubleshooting

### "command not found: gtc"

```bash
# Check if greentic is installed
which gtc
# If not, install:
cargo install --path ../greentic
```

### "Bundle not found"

```bash
# Ensure bundle exists
ls -la /tmp/test-bundle
# If not, init first:
gtc setup bundle init /tmp/test-bundle --name "Test"
```

### "Pack ref not found"

```bash
# Check pack exists
ls -la ../greentic-messaging-providers/packs/messaging-dummy/dist/
# If not, build:
cd ../greentic-messaging-providers/packs/messaging-dummy
greentic-pack build --in . --gtpack-out ./dist/messaging-dummy.gtpack
```

### Interactive wizard shows "Device not configured"

This happens when running in non-TTY environment (e.g., scripts). Use `--answers` instead:

```bash
# Generate template
gtc setup --dry-run --emit-answers /tmp/answers.json /tmp/test-bundle
# Edit and apply
gtc setup --answers /tmp/answers.json /tmp/test-bundle
```
