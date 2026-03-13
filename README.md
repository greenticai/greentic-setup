# Greentic Demo Bundles

Ready-to-run AI digital worker demos for common industry scenarios.

## Available Demos

| Bundle | Description | OCI Reference |
|--------|-------------|---------------|
| **Telecom** | Customer support, billing inquiries, service provisioning | `oci://ghcr.io/greenticai/demos/telecom.gtbundle` |
| **Banking** | Account management, loan processing, fraud alerts | `oci://ghcr.io/greenticai/demos/banking.gtbundle` |
| **Government** | Citizen services, permit applications, compliance | `oci://ghcr.io/greenticai/demos/government.gtbundle` |
| **Healthcare** | Patient intake, appointment scheduling, triage | `oci://ghcr.io/greenticai/demos/healthcare.gtbundle` |
| **Retail** | Product catalog, order tracking, returns | `oci://ghcr.io/greenticai/demos/retail.gtbundle` |

## Quick Start

### 1. Install

```bash
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install cargo-binstall (fast binary installer)
cargo install cargo-binstall

# Install gtc
cargo binstall gtc

# One-time setup
gtc install
```

### 2. Setup a Demo Bundle

```bash
# Interactive setup (prompts for required values)
gtc setup oci://ghcr.io/greenticai/demos/telecom.gtbundle

# Or use a pre-filled answers file
gtc setup oci://ghcr.io/greenticai/demos/telecom.gtbundle \
  --answers https://raw.githubusercontent.com/greenticai/demos/main/answers/telecom.json
```

> **Note:** If a required field is missing from the answers file, `gtc setup` will prompt you to fill it in interactively.

### 3. Start

```bash
gtc start ./telecom.gtbundle
```

## Configuration

### Answers File

Each demo includes a template answers file. Download and customize it before running setup:

```bash
# Generate an answers template
gtc setup oci://ghcr.io/greenticai/demos/telecom.gtbundle \
  --dry-run --emit-answers answers.json

# Edit answers.json with your values, then apply
gtc setup oci://ghcr.io/greenticai/demos/telecom.gtbundle \
  --answers answers.json
```

Example `answers.json`:

```json
{
  "greentic_setup_version": "1.0.0",
  "tenant": "demo",
  "env": "dev",
  "setup_answers": {
    "messaging-telegram": {
      "bot_token": "123456:ABC-your-bot-token",
      "public_base_url": "https://your-domain.example.com"
    }
  }
}
```

### Public Base URL

The `public_base_url` is the externally reachable URL for webhook callbacks (e.g., Telegram, Slack). Options:

| Method | Example |
|--------|---------|
| **Cloudflare Tunnel** | `cloudflared tunnel --url http://localhost:8080` |
| **ngrok** | `ngrok http 8080` |
| **Static** | Your server's public URL |

Set it in your answers file or provide it when prompted during `gtc setup`.

### Advanced Mode

To configure optional settings (e.g., default chat ID, custom webhook paths):

```bash
gtc setup oci://ghcr.io/greenticai/demos/telecom.gtbundle --advanced
```

## Messaging Providers

Each demo bundle supports multiple messaging channels:

| Provider | Setup Required |
|----------|---------------|
| Telegram | Bot token from [@BotFather](https://t.me/BotFather) |
| Slack | Bot token + Signing secret from [Slack API](https://api.slack.com/apps) |
| Teams | App ID + Password from [Azure Bot](https://portal.azure.com) |
| WebChat | Built-in (no external setup) |

## Troubleshooting

```bash
# Check bundle status
gtc setup ./telecom.gtbundle --dry-run

# Verify provider configuration
gtc bundle status --bundle ./telecom.gtbundle

# Enable debug logging
RUST_LOG=debug gtc start ./telecom.gtbundle
```

## Links

- [Greentic Documentation](https://greentic.ai/docs)
- [Report Issues](https://github.com/greenticai/demos/issues)
