# Greentic Bundle Demo Guide

Complete guide for creating, configuring, and running Greentic bundles.

## Table of Contents

- [Prerequisites](#prerequisites)
- [Quick Start](#quick-start)
- [Step-by-Step Guide](#step-by-step-guide)
- [Answers File Format](#answers-file-format)
- [Provider-Specific Setup](#provider-specific-setup)
- [Advanced Usage](#advanced-usage)
- [Troubleshooting](#troubleshooting)

---

## Prerequisites

```bash
# Verify all tools are installed
gtc doctor
```

Expected output:
```
greentic-dev: OK (greentic-dev 0.4.x)
greentic-operator: OK (greentic-operator 0.4.x)
greentic-setup: OK (greentic-setup 0.4.x)
```

---

## Quick Start

### Simple Workflow (Recommended)

```bash
# 1. Create bundle with a pack
gtc setup bundle init ./my-demo --name "My Demo"
gtc setup bundle add ./messaging-telegram.gtpack --bundle ./my-demo

# 2. Generate answers template
gtc setup --dry-run --emit-answers answers.json ./my-demo

# 3. Edit answers.json with your credentials

# 4. Run setup
gtc setup --answers answers.json ./my-demo

# 5. Start demo
gtc op demo start --bundle ./my-demo
```

### One-Liner Examples

```bash
# Preview setup (dry-run)
gtc setup --dry-run ./my-demo

# Generate answers template
gtc setup --dry-run --emit-answers answers.json ./my-demo

# Apply answers and setup
gtc setup --answers answers.json ./my-demo

# For production .gtbundle files
gtc setup --answers answers.json ./my-bundle.gtbundle
```

---

## Step-by-Step Guide

### 1. Initialize Bundle

```bash
gtc setup bundle init ./telegram-demo --name "Telegram Demo"
```

**Bundle Structure:**
```
telegram-demo/
├── greentic.demo.yaml      # Demo configuration
├── packs/                  # Pack storage
├── providers/              # Provider configs
├── seeds/                  # Seed data
├── resolved/               # Resolved manifests
└── tenants/                # Tenant configs
```

### 2. Add Packs

```bash
# From local file
gtc setup bundle add ./messaging-telegram.gtpack --bundle ./telegram-demo

# From OCI registry
gtc setup bundle add oci://ghcr.io/greentic-ai/packs/messaging-telegram:latest \
  --bundle ./telegram-demo
```

### 3. Generate Answers Template

```bash
gtc setup --dry-run --emit-answers answers.json ./telegram-demo
```

This creates `answers.json`:
```json
{
  "bundle_source": "./telegram-demo",
  "env": "dev",
  "tenant": "demo",
  "platform_setup": {
    "static_routes": {
      "public_web_enabled": false,
      "public_surface_policy": "disabled",
      "default_route_prefix_policy": "pack_declared",
      "tenant_path_policy": "pack_declared"
    }
  },
  "setup_answers": {
    "messaging-telegram": {
      "bot_token": "",
      "public_base_url": ""
    }
  }
}
```

### 4. Configure Answers

Edit `answers.json`:
```json
{
  "bundle_source": "./telegram-demo",
  "env": "dev",
  "tenant": "demo",
  "platform_setup": {
    "static_routes": {
      "public_web_enabled": true,
      "public_base_url": "https://abc123.ngrok.io",
      "public_surface_policy": "enabled",
      "default_route_prefix_policy": "pack_declared",
      "tenant_path_policy": "pack_declared"
    }
  },
  "setup_answers": {
    "messaging-telegram": {
      "bot_token": "123456789:ABCdefGHIjklMNOpqrsTUVwxyz",
      "public_base_url": "https://abc123.ngrok.io"
    }
  }
}
```

### 5. Run Setup

```bash
gtc setup --answers answers.json ./telegram-demo
```

### 6. Verify Bundle

```bash
gtc setup bundle status --bundle ./telegram-demo
```

### 7. Run Demo

```bash
gtc op demo start --bundle ./telegram-demo
```

---

## Answers File Format

### JSON Format

```json
{
  "bundle_source": "./my-bundle",
  "env": "dev",
  "tenant": "demo",
  "team": "default",
  "platform_setup": {
    "static_routes": {
      "public_web_enabled": false,
      "public_surface_policy": "disabled",
      "default_route_prefix_policy": "pack_declared",
      "tenant_path_policy": "pack_declared"
    }
  },
  "setup_answers": {
    "messaging-telegram": {
      "bot_token": "your-bot-token",
      "public_base_url": "https://your-domain.com"
    },
    "state-redis": {
      "redis_url": "redis://localhost:6379"
    }
  }
}
```

### YAML Format

```yaml
bundle_source: ./my-bundle
env: dev
tenant: demo
setup_answers:
  messaging-telegram:
    bot_token: "your-bot-token"
    public_base_url: "https://your-domain.com"
```

---

## Provider-Specific Setup

### Telegram

1. Create bot via [@BotFather](https://t.me/BotFather)
2. Get bot token
3. Setup public URL (ngrok/cloudflare tunnel)

```json
{
  "messaging-telegram": {
    "bot_token": "123456789:ABCdefGHIjklMNOpqrsTUVwxyz",
    "public_base_url": "https://abc123.ngrok.io"
  }
}
```

### Slack

```json
{
  "messaging-slack": {
    "bot_token": "xoxb-your-bot-token",
    "signing_secret": "your-signing-secret",
    "public_base_url": "https://your-domain.com"
  }
}
```

### Microsoft Teams

```json
{
  "messaging-teams": {
    "app_id": "your-app-id",
    "app_password": "your-app-password",
    "public_base_url": "https://your-domain.com"
  }
}
```

### Redis State

```json
{
  "state-redis": {
    "redis_url": "redis://localhost:6379",
    "redis_auth_enabled": false
  }
}
```

---

## Advanced Usage

### Bundle Subcommands

For advanced lifecycle management:

```bash
# Initialize bundle
gtc setup bundle init ./my-bundle --name "My Bundle"

# Add pack
gtc setup bundle add pack.gtpack --bundle ./my-bundle

# Setup specific provider
gtc setup bundle setup messaging-telegram --bundle ./my-bundle --answers telegram.json

# Update provider
gtc setup bundle update messaging-telegram --bundle ./my-bundle --answers telegram.json

# Remove provider
gtc setup bundle remove messaging-telegram --bundle ./my-bundle --force

# Build portable bundle
gtc setup bundle build --bundle ./my-bundle --out ./dist

# List packs
gtc setup bundle list --bundle ./my-bundle --domain messaging

# Show status
gtc setup bundle status --bundle ./my-bundle --format json
```

### Multi-Tenant Setup

```bash
gtc setup --tenant production --team ops --answers prod.json ./my-bundle
```

### CI/CD Integration

```bash
#!/bin/bash
set -e

# Create and setup bundle
gtc setup bundle init ./bundle --name "CI Demo"
gtc setup bundle add oci://ghcr.io/greentic-ai/packs/messaging-telegram:latest --bundle ./bundle

# Setup with environment-specific answers
gtc setup --answers ./config/${ENV}/answers.json ./bundle

# Build artifact
gtc setup bundle build --bundle ./bundle --out ./dist
```

---

## Troubleshooting

### Debug Mode

```bash
# Dry run
gtc setup --dry-run ./my-bundle

# With verbose output
gtc setup bundle setup --bundle ./my-bundle --answers answers.json --verbose
```

### Common Errors

| Error | Solution |
|-------|----------|
| `bundle not found` | Check bundle path exists |
| `failed to read answers file` | Check JSON/YAML syntax |
| `missing required field` | Add field to answers file |
| `interactive wizard not yet implemented` | Use `--answers <file>` |

### Reset Bundle

```bash
# Remove provider
gtc setup bundle remove messaging-telegram --bundle ./my-bundle --force

# Full reset
rm -rf ./my-bundle
gtc setup bundle init ./my-bundle --name "Fresh"
```

---

## Command Reference

### Simple Commands

| Command | Description |
|---------|-------------|
| `gtc setup <BUNDLE>` | Interactive wizard |
| `gtc setup --dry-run <BUNDLE>` | Preview without executing |
| `gtc setup --emit-answers <FILE> <BUNDLE>` | Generate answers template |
| `gtc setup --answers <FILE> <BUNDLE>` | Apply answers file |

### Bundle Subcommands

| Command | Description |
|---------|-------------|
| `gtc setup bundle init` | Initialize bundle |
| `gtc setup bundle add` | Add pack |
| `gtc setup bundle setup` | Run setup |
| `gtc setup bundle update` | Update config |
| `gtc setup bundle remove` | Remove provider |
| `gtc setup bundle build` | Build portable |
| `gtc setup bundle list` | List packs |
| `gtc setup bundle status` | Show status |

### Common Flags

| Flag | Description |
|------|-------------|
| `--dry-run` | Preview without changes |
| `--emit-answers <FILE>` | Generate template |
| `--answers <FILE>` | Apply answers |
| `--tenant <NAME>` | Tenant (default: demo) |
| `--team <NAME>` | Team (default: default) |
| `--env <NAME>` | Environment (default: dev) |
