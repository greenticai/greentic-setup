# Adaptive Cards Setup Guide

This guide explains how to use Adaptive Cards for secure, interactive bundle setup flows in messaging channels like Microsoft Teams, WebChat, and Slack.

## Overview

Adaptive Cards provide a rich, interactive way for users to configure providers without leaving their chat interface. The setup flow is:

1. Admin creates a card setup session via API
2. Server generates a signed setup URL and Adaptive Card
3. Card is sent to user in their messaging channel
4. User fills out the card and submits
5. Answers are validated and persisted securely
6. Multi-step flows show next card until complete

```
┌─────────────┐     ┌──────────────┐     ┌─────────────┐
│   Admin     │────▶│   Operator   │────▶│   User      │
│   (API)     │     │   (Server)   │     │   (Chat)    │
└─────────────┘     └──────────────┘     └─────────────┘
      │                    │                    │
      │ POST /card/create  │                    │
      │───────────────────▶│                    │
      │                    │   Send Card        │
      │                    │───────────────────▶│
      │                    │                    │
      │                    │   Submit Answers   │
      │                    │◀───────────────────│
      │                    │                    │
      │                    │   Next Card or     │
      │                    │   Complete         │
      │                    │───────────────────▶│
```

---

## Security Model

### 1. Session-Based Authentication

Each setup flow gets a unique session ID with expiration:

```json
{
  "session_id": "setup-1a2b3c4d5e6f",
  "created_at": 1709247600,
  "expires_at": 1709249400,
  "provider_id": "messaging-telegram",
  "tenant": "demo"
}
```

**Security properties:**
- Sessions expire after configurable TTL (default: 30 minutes)
- Each session is single-use per provider
- Session state is server-side only

### 2. HMAC-Signed Tokens

Setup URLs include cryptographically signed tokens:

```
https://operator.example.com/setup?session=setup-xxx&token=<signed>&provider=telegram
```

**Token structure:**
```
{base64(payload)}.{base64(signature)}

payload = "{session_id}.{expires_at}.{provider_id}"
signature = HMAC-SHA256(payload, signing_key)
```

**Security properties:**
- Tokens cannot be forged without the signing key
- Tokens are bound to specific session and provider
- Expiration is embedded in the token
- Constant-time comparison prevents timing attacks

### 3. mTLS for Submission

Card submissions go through the admin API which requires mTLS:

```bash
# Card action URL points to admin endpoint
POST https://localhost:9443/admin/card/submit
# Requires client certificate
```

### 4. Visibility-Aware Validation

Conditional questions (`visible_if`) are evaluated server-side:

```yaml
questions:
  - name: auth_method
    kind: string
    required: true

  - name: api_key
    kind: string
    required: true
    secret: true
    visible_if:
      field: auth_method
      eq: "api_key"
```

- Invisible fields are not validated
- Invisible fields are not persisted
- Server re-evaluates visibility on each submission

---

## Implementation

### Session Types

```rust
/// A setup session that tracks multi-step card-based onboarding.
pub struct CardSetupSession {
    /// Unique session ID.
    pub session_id: String,
    /// Bundle being configured.
    pub bundle_path: PathBuf,
    /// Provider being configured.
    pub provider_id: String,
    /// Tenant context.
    pub tenant: String,
    /// Team context.
    pub team: Option<String>,
    /// Answers collected so far.
    pub answers: HashMap<String, Value>,
    /// Current step index.
    pub current_step: usize,
    /// When this session was created (Unix timestamp).
    pub created_at: u64,
    /// When this session expires (Unix timestamp).
    pub expires_at: u64,
    /// Whether this session has been completed.
    pub completed: bool,
}
```

### Link Configuration

```rust
/// Configuration for setup link generation.
pub struct SetupLinkConfig {
    /// Base URL for the setup endpoint.
    pub base_url: String,
    /// Default TTL for setup sessions (seconds).
    pub ttl_secs: u64,
    /// Signing key for setup tokens (hex-encoded).
    pub signing_key: Option<String>,
}
```

### Result Types

```rust
/// Result of processing a card setup submission.
pub struct CardSetupResult {
    /// Whether setup is complete (all steps answered).
    pub complete: bool,
    /// The next card to render (if not complete).
    pub next_card: Option<Value>,
    /// Warnings from the setup process.
    pub warnings: Vec<String>,
    /// Keys that were persisted.
    pub persisted_keys: Vec<String>,
}
```

---

## API Usage

### Step 1: Create Session

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
    "setup_url": "https://operator.example.com/setup?session=setup-1a2b3c4d&token=eyJzZXNz...&provider=messaging-telegram",
    "card": {
      "type": "AdaptiveCard",
      "version": "1.5",
      "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
      "body": [
        {
          "type": "TextBlock",
          "text": "Telegram Provider Setup",
          "size": "Large",
          "weight": "Bolder"
        },
        {
          "type": "TextBlock",
          "text": "Configure your Telegram bot settings.",
          "wrap": true
        },
        {
          "type": "Input.Text",
          "id": "bot_token",
          "label": "Bot Token *",
          "placeholder": "123456:ABC-DEF...",
          "style": "password",
          "isRequired": true
        },
        {
          "type": "Input.Text",
          "id": "public_base_url",
          "label": "Public Base URL *",
          "placeholder": "https://your-server.com",
          "isRequired": true
        }
      ],
      "actions": [
        {
          "type": "Action.Submit",
          "title": "Submit",
          "data": {
            "session_id": "setup-1a2b3c4d",
            "action": "submit"
          }
        }
      ]
    }
  }
}
```

### Step 2: Send Card to User

The returned `card` JSON can be sent directly to the user's messaging channel:

**Microsoft Teams:**
```json
{
  "type": "message",
  "attachments": [
    {
      "contentType": "application/vnd.microsoft.card.adaptive",
      "content": { /* card from response */ }
    }
  ]
}
```

**WebChat (Bot Framework):**
```javascript
await context.sendActivity({
  attachments: [CardFactory.adaptiveCard(card)]
});
```

### Step 3: Handle Submission

When the user submits the card, the answers are sent to your webhook. Forward them to the admin API:

```bash
curl --cert client.crt --key client.key --cacert ca.crt \
  -X POST https://localhost:9443/admin/card/submit \
  -H "Content-Type: application/json" \
  -d '{
    "session_id": "setup-1a2b3c4d",
    "token": "eyJzZXNz...",
    "answers": {
      "bot_token": "123456:ABC-DEF",
      "public_base_url": "https://example.com"
    }
  }'
```

### Step 4: Handle Response

**If more steps needed:**
```json
{
  "success": true,
  "data": {
    "complete": false,
    "next_card": { /* next Adaptive Card */ },
    "warnings": []
  }
}
```
→ Send `next_card` to the user

**If complete:**
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
→ Send confirmation message to user

---

## Multi-Step Flows

For complex providers with many questions, the setup can be split into multiple steps:

```yaml
# Provider setup.yaml with steps
provider_id: complex-provider
version: 1
title: Complex Provider Setup
steps:
  - title: Authentication
    questions:
      - name: auth_method
        kind: string
        required: true
      - name: api_key
        kind: string
        secret: true
        visible_if:
          field: auth_method
          eq: "api_key"

  - title: Configuration
    questions:
      - name: webhook_url
        kind: string
        required: true
      - name: retry_count
        kind: number
        default: 3
```

The server tracks `current_step` in the session and returns the appropriate card for each step.

---

## Conditional Questions in Cards

Questions with `visible_if` are dynamically shown/hidden:

```yaml
questions:
  - name: use_proxy
    title: Use Proxy?
    kind: boolean

  - name: proxy_url
    title: Proxy URL
    kind: string
    visible_if:
      field: use_proxy
      eq: true
```

**Behavior:**
1. Initial card shows only `use_proxy` toggle
2. If user enables proxy, next card includes `proxy_url`
3. Server re-evaluates visibility on each submission

---

## Configuration

### Operator Config

```yaml
# greentic.operator.yaml
card_setup:
  # Base URL for setup links (must be publicly accessible)
  base_url: "https://operator.example.com"

  # Session TTL in seconds (default: 1800 = 30 minutes)
  ttl_secs: 1800

  # HMAC signing key (generate with: openssl rand -hex 32)
  signing_key: "your-256-bit-hex-key-here"
```

### Generating a Signing Key

```bash
# Generate a secure 256-bit key
openssl rand -hex 32
# Output: 4a7b9c3d2e1f0a8b7c6d5e4f3a2b1c0d...

# Or using Python
python3 -c "import secrets; print(secrets.token_hex(32))"
```

---

## Error Handling

### Session Expired

```json
{
  "success": false,
  "error": "Session expired. Please request a new setup link."
}
```

**Recovery:** Create a new session and send fresh card.

### Invalid Token

```json
{
  "success": false,
  "error": "Invalid or tampered token."
}
```

**Recovery:** This indicates tampering. Log and investigate.

### Validation Failed

```json
{
  "success": false,
  "error": "Validation failed",
  "data": {
    "errors": [
      {"field": "webhook_url", "message": "Must start with https://"}
    ]
  }
}
```

**Recovery:** Re-send the card with error messages displayed.

---

## Best Practices

### 1. Use Short TTLs

```yaml
ttl_secs: 900  # 15 minutes for sensitive setups
```

### 2. Always Use Signing Keys in Production

```yaml
signing_key: "${SETUP_SIGNING_KEY}"  # From environment
```

### 3. Validate on Submit, Not Just Display

The server re-validates all answers on submission, even if they were validated client-side.

### 4. Handle Multi-Step State

Track session progress and allow users to go back:

```json
{
  "actions": [
    {"type": "Action.Submit", "title": "Back", "data": {"action": "back"}},
    {"type": "Action.Submit", "title": "Next", "data": {"action": "submit"}}
  ]
}
```

### 5. Audit Logging

Log all setup completions for audit trail:

```
[2024-03-01 12:34:56] Setup completed: provider=telegram tenant=demo user=admin@example.com
```

---

## See Also

- [Admin API Reference](./admin-api.md) - Full endpoint documentation
- [mTLS Setup Guide](./mtls-setup.md) - Certificate configuration
- [Adaptive Cards Schema](https://adaptivecards.io/explorer/) - Card element reference
