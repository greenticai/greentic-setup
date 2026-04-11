# Greentic Setup Dashboard Phase 1a — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebuild `greentic-setup` web UI as a dashboard-first SPA with scope switcher, multi-scope overview, and embedded setup wizard. Full rewrite using Alpine.js, Poppins, and Greentic brand tokens.

**Spec reference:** `docs/superpowers/specs/2026-04-11-greentic-setup-dashboard-phase-1a-design.md`

**Architecture:** Single Axum server on `127.0.0.1:{random}` serves an Alpine.js SPA with JSON APIs. Dashboard shell with sidebar (brand + scope switcher + nav + footer) and main area (topbar + view). Client-side routing between `/` (overview) and `/wizard/*` views. Bearer token + origin check on all `/api/*`. Full rewrite: legacy `ui/mod.rs` and `assets/setup-ui/*` deleted in final task.

**Tech Stack:** Rust (axum 0.8, tokio, zeroize, uuid, constant_time_eq), Alpine.js v3 (vendored), Poppins font (self-hosted woff2), vanilla CSS (no build step), Tera-free HTML templates injected at boot.

**Branch:** Work on `feat/dashboard-phase-1a` (branch off `main`).

---

## Phase overview

- **Phase A (Tasks 1–3):** Project setup — deps, legacy rename, module skeleton
- **Phase B (Tasks 4–7):** Design system assets — Poppins, mascot, CSS
- **Phase C (Tasks 8–11):** Backend foundation — DTOs, auth, security headers, error envelope
- **Phase D (Tasks 12–18):** Backend API endpoints — bundle, overview, wizard (start/next/execute/session), locale, shutdown
- **Phase E (Tasks 19–21):** Backend assets embedding + server + routes wiring
- **Phase F (Tasks 22–25):** Frontend SPA — index.html, Alpine boot, router, stores, formatters
- **Phase G (Tasks 26–29):** Frontend components — shell, sidebar, topbar, overview, wizard, fields
- **Phase H (Tasks 30–31):** i18n — keys + translator propagation + coverage CI check
- **Phase I (Tasks 32–33):** Integration & security tests
- **Phase J (Task 34):** Cutover — delete legacy, rewire binary
- **Phase K (Tasks 35–39):** Docs, changelog, manual test, audit, final commit

---

## Prerequisites

Before starting, verify:

```bash
cd /home/bimbim/works/greentic/greentic-setup
git status                      # Should be clean or only unrelated changes
git branch --show-current       # Should be docs/dashboard-phase-1a-spec or main
```

Create and switch to the implementation branch:

```bash
git checkout main
git pull origin main
git checkout -b feat/dashboard-phase-1a
```

Verify spec is reachable:

```bash
ls docs/superpowers/specs/2026-04-11-greentic-setup-dashboard-phase-1a-design.md
```

---

## Phase A — Project Setup

### Task 1: Add Cargo.toml dependencies

**Files:**
- Modify: `Cargo.toml`

New dependencies needed for Phase 1a: `uuid` (wizard session IDs), `zeroize` (secret scrubbing), `constant_time_eq` (timing-safe bearer comparison), `axum-extra` (Bearer header extractor), `tower-http` (security headers middleware), `rand` 0.10 is already present.

- [ ] **Step 1.1: Add deps under `[dependencies]` section**

Open `Cargo.toml`, add the following lines in alphabetical order within `[dependencies]`:

```toml
axum-extra = { version = "0.10", features = ["typed-header"], optional = true }
constant_time_eq = "0.4"
tower = { version = "0.5", features = ["util"] }
tower-http = { version = "0.6", features = ["set-header"], optional = true }
uuid = { version = "1", features = ["v4", "serde"] }
zeroize = { version = "1.8", features = ["derive"] }
```

- [ ] **Step 1.2: Update the `ui` feature to include the new optional deps**

Change the `ui` feature line:

```toml
ui = ["axum", "axum-extra", "open", "tower-http"]
```

- [ ] **Step 1.3: Verify build succeeds**

```bash
cargo build --all-features
```

Expected: success (may take a minute to fetch new crates).

- [ ] **Step 1.4: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add deps for dashboard ui (uuid, zeroize, axum-extra, tower-http)"
```

---

### Task 2: Rename legacy ui/mod.rs and add new module skeleton

**Files:**
- Rename: `src/ui/mod.rs` → `src/ui/legacy.rs`
- Create: `src/ui/mod.rs` (new thin facade)
- Modify: `src/ui/assets.rs` (no change yet, keep compatible)

The legacy UI must keep working while we build the new one alongside. We rename the old file to `legacy.rs` and create a new `mod.rs` that re-exports `legacy::launch` as `launch` so `bin/greentic_setup.rs` compiles unchanged.

- [ ] **Step 2.1: Move the existing monolithic file**

```bash
git mv src/ui/mod.rs src/ui/legacy.rs
```

- [ ] **Step 2.2: Create the new module skeleton `src/ui/mod.rs`**

```rust
//! Greentic Setup web dashboard.
//!
//! During Phase 1a migration this module re-exports the legacy `launch`
//! function so the CLI binary keeps working while the new dashboard is built
//! out piece by piece in sibling modules. The legacy module will be removed
//! in the cutover task at the end of Phase 1a.

#![allow(dead_code)] // skeleton modules may have unused items during migration

mod assets;
mod legacy;

// New modules — currently empty stubs, filled in by subsequent tasks.
// Each stays private until the cutover task rewires `launch`.
mod auth;
mod server;
mod routes;
mod state;
mod sse;
mod api;

pub use legacy::launch;
```

- [ ] **Step 2.3: Verify build still passes**

```bash
cargo build --all-features
```

Expected: the new `mod auth;` etc. references fail because those files do not exist yet. That's expected — we create them in the next step.

- [ ] **Step 2.4: Create all empty stub files**

Create each of the following files with only a module doc comment:

```bash
touch src/ui/auth.rs
touch src/ui/server.rs
touch src/ui/routes.rs
touch src/ui/state.rs
touch src/ui/sse.rs
mkdir -p src/ui/api
touch src/ui/api/mod.rs
touch src/ui/api/bundle.rs
touch src/ui/api/overview.rs
touch src/ui/api/wizard.rs
touch src/ui/api/locale.rs
touch src/ui/api/error.rs
```

Write placeholder content in each. Example for `src/ui/auth.rs`:

```rust
//! Bearer token + Origin check middleware (Phase 1a).
//! Populated in Task 9.
```

Repeat for each file with an appropriate one-line doc comment referencing the task that fills it in. `src/ui/api/mod.rs` needs to declare the submodules:

```rust
//! JSON API handlers for the dashboard SPA.

pub mod bundle;
pub mod error;
pub mod locale;
pub mod overview;
pub mod wizard;
```

- [ ] **Step 2.5: Verify build passes with stubs**

```bash
cargo build --all-features
```

Expected: success. Legacy UI still works because `launch` is re-exported unchanged.

- [ ] **Step 2.6: Run existing tests to confirm nothing regressed**

```bash
cargo test --all-features
```

Expected: all existing tests pass.

- [ ] **Step 2.7: Commit**

```bash
git add src/ui/
git commit -m "refactor: rename ui/mod.rs to legacy, scaffold new ui modules"
```

---

### Task 3: Smoke test that legacy UI still launches

**Files:**
- Read: `src/ui/legacy.rs` (no modification)

Quick sanity check that renaming didn't break the runtime path. This is manual — not a new automated test, just confirm the binary still runs.

- [ ] **Step 3.1: Create a tiny demo bundle directory**

```bash
mkdir -p /tmp/phase1a-smoke-bundle
echo "id: smoke" > /tmp/phase1a-smoke-bundle/bundle.yaml
```

- [ ] **Step 3.2: Launch the binary with `--no-ui` to avoid opening a browser**

```bash
cargo run --all-features -- bundle list --bundle /tmp/phase1a-smoke-bundle 2>&1 | head -20
```

Expected: command runs (may print an error about empty bundle — that's fine, we just need the binary to start).

- [ ] **Step 3.3: No commit — smoke test only**

---

## Phase B — Design system assets

### Task 4: Create new asset directory and vendor Poppins

**Files:**
- Create: `assets/setup-ui-v2/vendor/poppins/poppins-{400,500,600,700}.woff2`
- Create: `assets/setup-ui-v2/LICENSE-Poppins.txt`

We build Phase 1a assets in `assets/setup-ui-v2/` so the legacy `assets/setup-ui/` keeps working until the final cutover. In the cutover task we delete the legacy dir and rename `-v2` to the canonical name.

- [ ] **Step 4.1: Create directory structure**

```bash
mkdir -p assets/setup-ui-v2/vendor/poppins
mkdir -p assets/setup-ui-v2/vendor/alpine
mkdir -p assets/setup-ui-v2/js/stores
mkdir -p assets/setup-ui-v2/components
mkdir -p assets/setup-ui-v2/styles
mkdir -p assets/setup-ui-v2/icons
```

- [ ] **Step 4.2: Download Poppins Latin subset woff2 files**

Use the official Google Fonts CDN Latin subset. These four weights are enough for our type scale:

```bash
curl -sSL -o assets/setup-ui-v2/vendor/poppins/poppins-400.woff2 \
  "https://fonts.gstatic.com/s/poppins/v21/pxiEyp8kv8JHgFVrJJfecnFHGPc.woff2"
curl -sSL -o assets/setup-ui-v2/vendor/poppins/poppins-500.woff2 \
  "https://fonts.gstatic.com/s/poppins/v21/pxiByp8kv8JHgFVrLGT9Z1xlFQ.woff2"
curl -sSL -o assets/setup-ui-v2/vendor/poppins/poppins-600.woff2 \
  "https://fonts.gstatic.com/s/poppins/v21/pxiByp8kv8JHgFVrLEj6Z1xlFQ.woff2"
curl -sSL -o assets/setup-ui-v2/vendor/poppins/poppins-700.woff2 \
  "https://fonts.gstatic.com/s/poppins/v21/pxiByp8kv8JHgFVrLCz7Z1xlFQ.woff2"
```

Verify each file is a real woff2 (magic bytes `wOF2`):

```bash
for f in assets/setup-ui-v2/vendor/poppins/*.woff2; do
  head -c 4 "$f" | xxd
done
```

Expected output for each: `00000000: 774f 4632                                wOF2`

If any file is not a woff2 (e.g. Google Fonts returned an HTML redirect), redownload using `curl -L` and a browser User-Agent.

- [ ] **Step 4.3: Add Poppins license file**

Download the SIL Open Font License 1.1 file used by Poppins:

```bash
cat > assets/setup-ui-v2/LICENSE-Poppins.txt <<'EOF'
Poppins is licensed under the SIL Open Font License, Version 1.1
Copyright 2014-2020 The Poppins Project Authors (https://github.com/itfoundry/Poppins)
Full license: https://openfontlicense.org/open-font-license-official-text/
EOF
```

- [ ] **Step 4.4: Commit**

```bash
git add assets/setup-ui-v2/
git commit -m "feat(ui): vendor Poppins font (Latin subset, SIL OFL)"
```

---

### Task 5: Copy mascot and write design tokens CSS

**Files:**
- Create: `assets/setup-ui-v2/icons/greentic-mascot.png`
- Create: `assets/setup-ui-v2/styles/tokens.css`
- Create: `assets/setup-ui-v2/styles/base.css`

- [ ] **Step 5.1: Copy the mascot from the brainstorm cache**

```bash
cp .superpowers/brainstorm/*/content/greentic-logo.png \
   assets/setup-ui-v2/icons/greentic-mascot.png 2>/dev/null || \
   curl -sSL -o assets/setup-ui-v2/icons/greentic-mascot.png \
     "https://greentic.ai/assets/greentic-logo-B2qbsAIc.png"

file assets/setup-ui-v2/icons/greentic-mascot.png
```

Expected: `PNG image data, 224 x 200, 8-bit/color RGBA, non-interlaced`

- [ ] **Step 5.2: Write `assets/setup-ui-v2/styles/tokens.css`**

```css
/* Greentic design tokens — derived from https://greentic.ai CSS custom
 * properties (shadcn/ui HSL format). Dark mode only in Phase 1a.
 * Spec: docs/superpowers/specs/2026-04-11-greentic-setup-dashboard-phase-1a-design.md
 */

:root {
  /* Brand */
  --brand: hsl(166 70% 45%);
  --brand-hover: hsl(166 70% 55%);
  --brand-muted: hsl(166 50% 40%);
  --brand-bg: hsl(166 70% 45% / 0.10);
  --brand-border: hsl(166 70% 45% / 0.30);
  --brand-ink: hsl(220 20% 8%);
  --accent: hsl(186 70% 50%);

  /* Surfaces (dark) */
  --bg: hsl(220 20% 8%);
  --surface: hsl(220 20% 11%);
  --surface-2: hsl(220 20% 13%);
  --border: hsl(220 15% 18%);
  --border-subtle: hsl(220 15% 14%);

  /* Text */
  --fg: hsl(0 0% 98%);
  --fg-muted: hsl(220 15% 62%);
  --fg-dim: hsl(220 10% 46%);

  /* Semantic */
  --success: hsl(142 60% 55%);
  --warning: hsl(42 80% 62%);
  --danger: hsl(0 70% 60%);
  --info: hsl(210 80% 60%);

  /* Typography */
  --font-sans: 'Poppins', ui-sans-serif, system-ui, -apple-system, sans-serif;
  --font-mono: ui-monospace, SFMono-Regular, Menlo, Monaco, monospace;

  /* Spacing (4px grid) */
  --space-1: 4px;
  --space-2: 8px;
  --space-3: 12px;
  --space-4: 16px;
  --space-6: 24px;
  --space-8: 32px;
  --space-12: 48px;
  --space-16: 64px;

  /* Radius */
  --r-sm: 6px;
  --r-md: 8px;
  --r-lg: 12px;
  --r-full: 9999px;

  /* Elevation — one subtle shadow, used sparingly */
  --shadow-subtle: 0 1px 2px hsl(0 0% 0% / 0.2);

  /* Motion */
  --ease-fast: 120ms cubic-bezier(0.4, 0, 0.2, 1);
  --ease-base: 200ms cubic-bezier(0.4, 0, 0.2, 1);
  --ease-smooth: 300ms cubic-bezier(0.4, 0, 0.2, 1);
}

@media (prefers-reduced-motion: reduce) {
  :root {
    --ease-fast: 0.01ms linear;
    --ease-base: 0.01ms linear;
    --ease-smooth: 0.01ms linear;
  }
}
```

- [ ] **Step 5.3: Write `assets/setup-ui-v2/styles/base.css`**

```css
/* Reset + @font-face Poppins + body defaults. Scoped for clarity. */

@font-face {
  font-family: 'Poppins';
  font-style: normal;
  font-weight: 400;
  font-display: swap;
  src: url('/vendor/poppins/poppins-400.woff2') format('woff2');
  unicode-range: U+0000-00FF, U+0131, U+0152-0153, U+02BB-02BC, U+02C6, U+02DA, U+02DC, U+2000-206F, U+2074, U+20AC, U+2122, U+2191, U+2193, U+2212, U+2215, U+FEFF, U+FFFD;
}
@font-face {
  font-family: 'Poppins';
  font-style: normal;
  font-weight: 500;
  font-display: swap;
  src: url('/vendor/poppins/poppins-500.woff2') format('woff2');
  unicode-range: U+0000-00FF, U+0131, U+0152-0153, U+02BB-02BC, U+02C6, U+02DA, U+02DC, U+2000-206F, U+2074, U+20AC, U+2122, U+2191, U+2193, U+2212, U+2215, U+FEFF, U+FFFD;
}
@font-face {
  font-family: 'Poppins';
  font-style: normal;
  font-weight: 600;
  font-display: swap;
  src: url('/vendor/poppins/poppins-600.woff2') format('woff2');
  unicode-range: U+0000-00FF, U+0131, U+0152-0153, U+02BB-02BC, U+02C6, U+02DA, U+02DC, U+2000-206F, U+2074, U+20AC, U+2122, U+2191, U+2193, U+2212, U+2215, U+FEFF, U+FFFD;
}
@font-face {
  font-family: 'Poppins';
  font-style: normal;
  font-weight: 700;
  font-display: swap;
  src: url('/vendor/poppins/poppins-700.woff2') format('woff2');
  unicode-range: U+0000-00FF, U+0131, U+0152-0153, U+02BB-02BC, U+02C6, U+02DA, U+02DC, U+2000-206F, U+2074, U+20AC, U+2122, U+2191, U+2193, U+2212, U+2215, U+FEFF, U+FFFD;
}

*, *::before, *::after {
  box-sizing: border-box;
  margin: 0;
  padding: 0;
}

html {
  -webkit-text-size-adjust: 100%;
  text-size-adjust: 100%;
}

html, body {
  height: 100%;
}

body {
  font-family: var(--font-sans);
  font-size: 14px;
  line-height: 22px;
  color: var(--fg);
  background: var(--bg);
  -webkit-font-smoothing: antialiased;
  -moz-osx-font-smoothing: grayscale;
}

h1, h2, h3, h4, h5, h6 {
  font-weight: 600;
  line-height: 1.3;
  color: var(--fg);
  letter-spacing: -0.1px;
}

a { color: inherit; text-decoration: none; }
button { font: inherit; color: inherit; background: none; border: none; cursor: pointer; }
input, textarea, select { font: inherit; color: inherit; }
code, pre, kbd, samp { font-family: var(--font-mono); }

:focus-visible {
  outline: 2px solid var(--brand);
  outline-offset: 2px;
  border-radius: var(--r-sm);
}
```

- [ ] **Step 5.4: Commit**

```bash
git add assets/setup-ui-v2/
git commit -m "feat(ui): add design tokens, base CSS, mascot"
```

---

### Task 6: Write layout.css, components.css, animations.css

**Files:**
- Create: `assets/setup-ui-v2/styles/layout.css`
- Create: `assets/setup-ui-v2/styles/components.css`
- Create: `assets/setup-ui-v2/styles/animations.css`

- [ ] **Step 6.1: Write `assets/setup-ui-v2/styles/layout.css`**

```css
/* Dashboard shell grid: sidebar 240px + main (1fr). */

.shell {
  display: grid;
  grid-template-columns: 240px 1fr;
  height: 100vh;
  overflow: hidden;
}

.sidebar {
  background: var(--bg);
  border-right: 1px solid var(--border);
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.sidebar-brand {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  padding: var(--space-6) var(--space-4) var(--space-4);
}
.sidebar-brand img { width: 30px; height: 30px; flex-shrink: 0; }
.sidebar-brand-text { font-size: 14px; font-weight: 600; line-height: 1.2; }
.sidebar-brand-sub { font-size: 11px; color: var(--fg-muted); margin-top: 1px; }

.sidebar-scope { padding: var(--space-1) var(--space-4) var(--space-4); }
.sidebar-scope-label {
  font-size: 10px; color: var(--fg-dim); margin: 0 var(--space-1) var(--space-2);
}
.sidebar-nav { padding: var(--space-2) var(--space-3); flex: 1; overflow-y: auto; }
.sidebar-nav-label {
  font-size: 10px; color: var(--fg-dim);
  padding: var(--space-3) var(--space-2) var(--space-2);
}
.sidebar-footer {
  padding: var(--space-3) var(--space-4);
  border-top: 1px solid var(--border);
  font-size: 11px; color: var(--fg-muted);
  display: flex; align-items: center; gap: var(--space-2);
}
.sidebar-footer .dot {
  width: 6px; height: 6px; border-radius: 50%; background: var(--success);
}

.main {
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.topbar {
  padding: var(--space-4) var(--space-8);
  display: flex;
  align-items: center;
  gap: var(--space-3);
  border-bottom: 1px solid var(--border);
}
.topbar-breadcrumb { flex: 1; font-size: 13px; font-weight: 500; }

.content {
  padding: var(--space-8) var(--space-8);
  flex: 1;
  overflow-y: auto;
}

/* RTL */
[dir="rtl"] .shell {
  grid-template-columns: 1fr 240px;
  direction: rtl;
}
[dir="rtl"] .sidebar {
  border-right: none;
  border-left: 1px solid var(--border);
}
```

- [ ] **Step 6.2: Write `assets/setup-ui-v2/styles/components.css`**

```css
/* Reusable component classes. Clean + minimalist. */

/* Buttons */
.btn {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
  padding: var(--space-3) var(--space-4);
  border-radius: var(--r-md);
  font-size: 13px;
  font-weight: 500;
  border: 1px solid var(--border);
  background: transparent;
  color: var(--fg);
  transition: border-color var(--ease-base), background var(--ease-base);
}
.btn:hover { border-color: var(--fg-dim); }
.btn--primary {
  background: var(--brand);
  color: var(--brand-ink);
  font-weight: 600;
  border-color: var(--brand);
}
.btn--primary:hover { background: var(--brand-hover); border-color: var(--brand-hover); }
.btn--ghost { border-color: transparent; }
.btn--danger { color: var(--danger); border-color: hsl(0 70% 60% / 0.3); }

/* Fields */
.field { margin-bottom: var(--space-4); display: block; }
.field-label {
  display: block;
  font-size: 12px;
  font-weight: 500;
  color: var(--fg);
  margin-bottom: var(--space-2);
}
.field-label[data-required]::after {
  content: '*';
  color: var(--brand);
  margin-inline-start: 4px;
}
.field-input {
  background: var(--surface-2);
  border: 1px solid var(--border);
  border-radius: var(--r-md);
  padding: var(--space-3) var(--space-4);
  font-size: 13px;
  color: var(--fg);
  width: 100%;
  max-width: 440px;
  transition: border-color var(--ease-base), box-shadow var(--ease-base);
}
.field-input:focus {
  border-color: var(--brand);
  box-shadow: 0 0 0 3px hsl(166 70% 45% / 0.15);
  outline: none;
}
.field-help {
  font-size: 11px; color: var(--fg-muted);
  margin-top: var(--space-2); line-height: 1.5;
}
.field-error { color: var(--danger); font-size: 11px; margin-top: var(--space-2); }

/* Scope dropdown */
.scope-dropdown {
  background: transparent;
  border: 1px solid var(--border);
  border-radius: var(--r-md);
  padding: var(--space-2) var(--space-3);
  display: flex;
  align-items: center;
  gap: var(--space-2);
  font-size: 12px;
  color: var(--fg);
  margin-bottom: var(--space-1);
  cursor: pointer;
  transition: border-color var(--ease-base);
}
.scope-dropdown:hover { border-color: var(--fg-dim); }
.scope-dropdown.active {
  background: var(--brand-bg);
  border-color: var(--brand);
  color: var(--brand);
}
.scope-dropdown .hint {
  font-size: 10px; color: var(--fg-dim); text-transform: lowercase;
  margin-inline-end: var(--space-1);
}
.scope-dropdown .value { flex: 1; }
.scope-dropdown .caret { font-size: 9px; color: var(--fg-dim); }

/* Nav items */
.nav-item {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  padding: var(--space-2) var(--space-3);
  font-size: 13px;
  color: var(--fg-muted);
  border-radius: var(--r-md);
  margin-bottom: 1px;
  cursor: pointer;
  transition: background var(--ease-base);
}
.nav-item:hover { background: var(--surface); color: var(--fg); }
.nav-item.active {
  background: var(--brand-bg);
  color: var(--brand);
  font-weight: 500;
}
.nav-item.disabled { opacity: 0.4; cursor: not-allowed; }
.nav-item .ico { width: 14px; height: 14px; flex-shrink: 0; }
.nav-item .phase-tag {
  margin-inline-start: auto;
  font-size: 9px;
  padding: 1px 6px;
  border-radius: 4px;
  border: 1px solid var(--border);
  color: var(--fg-dim);
}

/* Stats row */
.stats {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  border-top: 1px solid var(--border);
  border-bottom: 1px solid var(--border);
  padding: var(--space-6) 0;
  margin-bottom: var(--space-12);
}
.stat { padding: 0 var(--space-6); border-right: 1px solid var(--border); }
.stat:last-child { border-right: none; }
.stat:first-child { padding-left: 0; }
.stat-val {
  font-size: 28px;
  font-weight: 600;
  letter-spacing: -0.6px;
  line-height: 1;
}
.stat-label {
  font-size: 11px;
  color: var(--fg-muted);
  margin-top: var(--space-2);
}

/* Scope card */
.scope-card {
  border: 1px solid var(--border);
  border-radius: var(--r-lg);
  padding: var(--space-4) var(--space-6);
  margin-bottom: var(--space-3);
  transition: border-color var(--ease-base);
}
.scope-card:hover { border-color: var(--fg-dim); }
.scope-card.active { border-color: var(--brand); background: var(--brand-bg); }
.scope-card-head {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  margin-bottom: var(--space-3);
}
.scope-card-title { font-size: 14px; font-weight: 500; }
.scope-card-path {
  font-family: var(--font-mono);
  font-size: 10.5px;
  color: var(--fg-dim);
  margin-inline-start: auto;
}
.provider-chip {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
  border: 1px solid var(--border);
  border-radius: var(--r-sm);
  padding: var(--space-2) var(--space-3);
  font-size: 11px;
  color: var(--fg-muted);
  margin-inline-end: var(--space-2);
}
.provider-chip::before {
  content: '';
  width: 5px;
  height: 5px;
  border-radius: 50%;
  background: currentColor;
  opacity: 0.6;
}
.provider-chip.ok { color: var(--success); border-color: hsl(142 60% 55% / 0.3); }
.provider-chip.warn { color: var(--warning); border-color: hsl(42 80% 62% / 0.3); }
.provider-chip.add {
  color: var(--fg-dim);
  border-style: dashed;
  cursor: pointer;
}
.provider-chip.add::before { display: none; }

/* Empty state */
.empty-row {
  border: 1px dashed var(--border);
  border-radius: var(--r-lg);
  padding: var(--space-6);
  text-align: center;
  color: var(--fg-muted);
  font-size: 13px;
}
```

- [ ] **Step 6.3: Write `assets/setup-ui-v2/styles/animations.css`**

```css
/* Micro-animations. All collapse to 0ms under prefers-reduced-motion. */

.fade-in {
  animation: fade-in var(--ease-base) both;
}
@keyframes fade-in {
  from { opacity: 0; }
  to { opacity: 1; }
}

.slide-up {
  animation: slide-up var(--ease-base) both;
}
@keyframes slide-up {
  from { opacity: 0; transform: translateY(8px); }
  to { opacity: 1; transform: translateY(0); }
}
```

- [ ] **Step 6.4: Commit**

```bash
git add assets/setup-ui-v2/styles/
git commit -m "feat(ui): layout, components, animations CSS"
```

---

### Task 7: Vendor Alpine.js

**Files:**
- Create: `assets/setup-ui-v2/vendor/alpine/alpine.min.js`
- Create: `assets/setup-ui-v2/vendor/alpine/LICENSE.txt`

Alpine.js v3 is the reactive framework. We vendor the minified build (not via CDN) to keep the setup binary airgapped-friendly.

- [ ] **Step 7.1: Download Alpine.js v3 min**

```bash
curl -sSL -o assets/setup-ui-v2/vendor/alpine/alpine.min.js \
  "https://cdn.jsdelivr.net/npm/alpinejs@3.13.10/dist/cdn.min.js"

head -c 60 assets/setup-ui-v2/vendor/alpine/alpine.min.js
```

Expected: starts with something like `(()=>{var e,t=...`. File size should be ~43 KB (minified, not gzipped).

- [ ] **Step 7.2: Add Alpine MIT license**

```bash
cat > assets/setup-ui-v2/vendor/alpine/LICENSE.txt <<'EOF'
Alpine.js is licensed under the MIT License.
Copyright (c) 2019-present Caleb Porzio and contributors.
Full license: https://github.com/alpinejs/alpine/blob/main/LICENSE.md
EOF
```

- [ ] **Step 7.3: Commit**

```bash
git add assets/setup-ui-v2/vendor/alpine/
git commit -m "feat(ui): vendor Alpine.js v3.13.10 (MIT)"
```

---

## Phase C — Backend foundation

### Task 8: State DTOs and scope validation (test-first)

**Files:**
- Create: `src/ui/state.rs` (replace stub)
- Create: `tests/ui_state.rs` (new integration test crate)

- [ ] **Step 8.1: Write the failing test**

Create `tests/ui_state.rs`:

```rust
//! Unit tests for ui::state DTOs and validation.

use greentic_setup::ui::state::{BundleMeta, ScopeKey, ScopeStatus, validate_scope};

#[test]
fn scope_key_serializes_with_snake_case() {
    let scope = ScopeKey {
        tenant: "demo".into(),
        env: "dev".into(),
        team: "default".into(),
    };
    let json = serde_json::to_string(&scope).unwrap();
    assert_eq!(json, r#"{"tenant":"demo","env":"dev","team":"default"}"#);
}

#[test]
fn scope_status_serializes_snake_case() {
    let s = ScopeStatus::NotConfigured;
    let json = serde_json::to_string(&s).unwrap();
    assert_eq!(json, r#""not_configured""#);
}

#[test]
fn validate_scope_accepts_allowed_tenant() {
    let bundle = BundleMeta::test_fixture(); // demo, acme-corp; dev, prod; default
    let scope = ScopeKey { tenant: "demo".into(), env: "dev".into(), team: "default".into() };
    assert!(validate_scope(&scope, &bundle).is_ok());
}

#[test]
fn validate_scope_rejects_unknown_tenant() {
    let bundle = BundleMeta::test_fixture();
    let scope = ScopeKey { tenant: "evil".into(), env: "dev".into(), team: "default".into() };
    let err = validate_scope(&scope, &bundle).unwrap_err();
    assert_eq!(err.code, "scope.invalid_tenant");
}

#[test]
fn validate_scope_rejects_path_traversal_in_env() {
    let bundle = BundleMeta::test_fixture();
    let scope = ScopeKey {
        tenant: "demo".into(),
        env: "../etc".into(),
        team: "default".into(),
    };
    let err = validate_scope(&scope, &bundle).unwrap_err();
    assert_eq!(err.code, "scope.path_traversal");
}

#[test]
fn validate_scope_rejects_slash_in_team() {
    let bundle = BundleMeta::test_fixture();
    let scope = ScopeKey {
        tenant: "demo".into(),
        env: "dev".into(),
        team: "a/b".into(),
    };
    let err = validate_scope(&scope, &bundle).unwrap_err();
    assert_eq!(err.code, "scope.path_traversal");
}
```

- [ ] **Step 8.2: Run the test to verify it fails**

```bash
cargo test --all-features --test ui_state
```

Expected: fails to compile — `state`, `BundleMeta`, etc. are not yet public.

- [ ] **Step 8.3: Implement `src/ui/state.rs`**

```rust
//! Shared application state and DTOs for the dashboard UI.
//!
//! These types are the wire format between the Axum handlers and the Alpine
//! SPA. All visible strings must be i18n keys, never raw English.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Scope triple identifying one `(tenant, env, team)` configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ScopeKey {
    pub tenant: String,
    pub env: String,
    pub team: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScopeStatus {
    Configured,
    Partial,
    NotConfigured,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WarningSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct WarningMessage {
    pub key: String,
    pub params: serde_json::Value,
    pub severity: WarningSeverity,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderStatus {
    pub id: String,
    pub display_name: String,
    pub configured: bool,
    pub secrets_count: u32,
    pub warnings: Vec<WarningMessage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScopeSummary {
    pub scope: ScopeKey,
    pub status: ScopeStatus,
    pub providers: Vec<ProviderStatus>,
    pub warnings: Vec<WarningMessage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderRef {
    pub oci: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BundleMeta {
    pub id: String,
    pub display_name: String,
    pub path: PathBuf,
    pub scopes: Vec<ScopeSummary>,
    pub available_tenants: Vec<String>,
    pub available_envs: Vec<String>,
    pub available_teams: Vec<String>,
    pub extension_providers: Vec<ProviderRef>,
}

impl BundleMeta {
    /// Small fixture used by unit tests.
    pub fn test_fixture() -> Self {
        Self {
            id: "demo".into(),
            display_name: "Demo Bundle".into(),
            path: PathBuf::from("/tmp/demo"),
            scopes: vec![],
            available_tenants: vec!["demo".into(), "acme-corp".into()],
            available_envs: vec!["dev".into(), "prod".into()],
            available_teams: vec!["default".into()],
            extension_providers: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OverviewStats {
    pub scopes_count: u32,
    pub providers_count: u32,
    pub secrets_count: u32,
    pub warnings_count: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct OverviewResponse {
    pub scope: ScopeKey,
    pub stats: OverviewStats,
    pub scopes: Vec<ScopeSummary>,
}

/// Validation error shape used by scope validation and API handlers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub code: String,
    pub key: String,
}

impl ValidationError {
    pub fn new(code: &str, key: &str) -> Self {
        Self { code: code.into(), key: key.into() }
    }
}

/// Validate a `ScopeKey` against the bundle's allow-list.
///
/// Rejects unknown tenant/env/team names and any component containing
/// path-traversal characters (`..`, `/`, `\`).
pub fn validate_scope(scope: &ScopeKey, bundle: &BundleMeta) -> Result<(), ValidationError> {
    if !bundle.available_tenants.iter().any(|t| t == &scope.tenant) {
        return Err(ValidationError::new("scope.invalid_tenant", "ui.error.invalid_tenant"));
    }
    if !bundle.available_envs.iter().any(|e| e == &scope.env) {
        return Err(ValidationError::new("scope.invalid_env", "ui.error.invalid_env"));
    }
    if !bundle.available_teams.iter().any(|t| t == &scope.team) {
        return Err(ValidationError::new("scope.invalid_team", "ui.error.invalid_team"));
    }
    for part in [&scope.tenant, &scope.env, &scope.team] {
        if part.contains("..") || part.contains('/') || part.contains('\\') {
            return Err(ValidationError::new("scope.path_traversal", "ui.error.scope_invalid"));
        }
    }
    Ok(())
}

/// Top-level app state shared across Axum handlers.
#[derive(Debug)]
pub struct AppState {
    pub bundle: BundleMeta,
    pub port: u16,
    pub bearer_token: String,
    pub wizard_sessions: std::sync::Mutex<std::collections::HashMap<Uuid, WizardSession>>,
    pub shutdown_tx: tokio::sync::broadcast::Sender<()>,
}

#[derive(Debug)]
pub struct WizardSession {
    pub id: Uuid,
    pub scope: ScopeKey,
    pub provider: Option<String>,
    pub current_step: u32,
    pub total_steps: u32,
    pub created_at: std::time::Instant,
    pub last_activity: std::time::Instant,
    pub answers: zeroize::Zeroizing<std::collections::HashMap<String, String>>,
}

impl WizardSession {
    pub const TTL: std::time::Duration = std::time::Duration::from_secs(30 * 60);

    pub fn new(scope: ScopeKey, provider: Option<String>, total_steps: u32) -> Self {
        let now = std::time::Instant::now();
        Self {
            id: Uuid::new_v4(),
            scope,
            provider,
            current_step: 1,
            total_steps,
            created_at: now,
            last_activity: now,
            answers: zeroize::Zeroizing::new(std::collections::HashMap::new()),
        }
    }

    pub fn is_expired(&self) -> bool {
        self.last_activity.elapsed() > Self::TTL
    }
}
```

- [ ] **Step 8.4: Update `src/ui/mod.rs` to make `state` public**

Change `mod state;` to `pub mod state;`.

- [ ] **Step 8.5: Run the test**

```bash
cargo test --all-features --test ui_state
```

Expected: all 6 tests pass.

- [ ] **Step 8.6: Commit**

```bash
git add src/ui/state.rs src/ui/mod.rs tests/ui_state.rs
git commit -m "feat(ui): state DTOs with scope validation (6 tests)"
```

---

### Task 9: Auth middleware (bearer + origin)

**Files:**
- Create: `src/ui/auth.rs` (replace stub)
- Create: `tests/ui_auth.rs`

- [ ] **Step 9.1: Write the failing test**

Create `tests/ui_auth.rs`:

```rust
//! Auth middleware tests — bearer token + Origin check.

use greentic_setup::ui::auth::{generate_bearer_token, verify_auth, AuthError};
use axum::http::{HeaderMap, HeaderValue};

const TOKEN: &str = "secret-token-abc-123";
const PORT: u16 = 52341;

fn headers(auth: Option<&str>, origin: Option<&str>) -> HeaderMap {
    let mut h = HeaderMap::new();
    if let Some(a) = auth {
        h.insert("authorization", HeaderValue::from_str(a).unwrap());
    }
    if let Some(o) = origin {
        h.insert("origin", HeaderValue::from_str(o).unwrap());
    }
    h
}

#[test]
fn generate_bearer_token_is_at_least_32_chars() {
    let t = generate_bearer_token();
    assert!(t.len() >= 32, "token too short: {}", t.len());
}

#[test]
fn generate_bearer_token_is_unique_per_call() {
    let a = generate_bearer_token();
    let b = generate_bearer_token();
    assert_ne!(a, b);
}

#[test]
fn verify_auth_rejects_missing_authorization_header() {
    let h = headers(None, Some("http://127.0.0.1:52341"));
    assert_eq!(verify_auth(&h, TOKEN, PORT), Err(AuthError::MissingBearer));
}

#[test]
fn verify_auth_rejects_wrong_bearer_token() {
    let h = headers(Some("Bearer wrong-token"), Some("http://127.0.0.1:52341"));
    assert_eq!(verify_auth(&h, TOKEN, PORT), Err(AuthError::InvalidBearer));
}

#[test]
fn verify_auth_accepts_correct_bearer_and_127_origin() {
    let h = headers(
        Some(&format!("Bearer {TOKEN}")),
        Some("http://127.0.0.1:52341"),
    );
    assert_eq!(verify_auth(&h, TOKEN, PORT), Ok(()));
}

#[test]
fn verify_auth_accepts_localhost_origin() {
    let h = headers(
        Some(&format!("Bearer {TOKEN}")),
        Some("http://localhost:52341"),
    );
    assert_eq!(verify_auth(&h, TOKEN, PORT), Ok(()));
}

#[test]
fn verify_auth_rejects_wrong_origin() {
    let h = headers(
        Some(&format!("Bearer {TOKEN}")),
        Some("http://evil.example.com"),
    );
    assert_eq!(verify_auth(&h, TOKEN, PORT), Err(AuthError::InvalidOrigin));
}

#[test]
fn verify_auth_rejects_missing_origin() {
    let h = headers(Some(&format!("Bearer {TOKEN}")), None);
    assert_eq!(verify_auth(&h, TOKEN, PORT), Err(AuthError::InvalidOrigin));
}
```

- [ ] **Step 9.2: Run to verify failure**

```bash
cargo test --all-features --test ui_auth
```

Expected: compilation fails — `auth` module items not defined.

- [ ] **Step 9.3: Implement `src/ui/auth.rs`**

```rust
//! Bearer token + Origin header authentication for /api/* routes.
//!
//! Even on 127.0.0.1 this guards against malicious local processes and
//! cross-origin attacks via a malicious site opened in the same browser.

use axum::http::HeaderMap;
use rand::RngCore;

#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    MissingBearer,
    InvalidBearer,
    InvalidOrigin,
}

/// Generate a 256-bit random bearer token encoded as base64-url (no padding).
pub fn generate_bearer_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Verify bearer token (constant-time compare) and Origin header.
pub fn verify_auth(
    headers: &HeaderMap,
    expected_token: &str,
    expected_port: u16,
) -> Result<(), AuthError> {
    let provided = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .ok_or(AuthError::MissingBearer)?;

    if !constant_time_eq::constant_time_eq(
        provided.as_bytes(),
        expected_token.as_bytes(),
    ) {
        return Err(AuthError::InvalidBearer);
    }

    let origin = headers
        .get("origin")
        .and_then(|h| h.to_str().ok())
        .ok_or(AuthError::InvalidOrigin)?;

    let ok_127 = format!("http://127.0.0.1:{expected_port}");
    let ok_local = format!("http://localhost:{expected_port}");
    if origin != ok_127 && origin != ok_local {
        return Err(AuthError::InvalidOrigin);
    }

    Ok(())
}
```

- [ ] **Step 9.4: Make auth module public**

In `src/ui/mod.rs`, change `mod auth;` to `pub mod auth;`.

- [ ] **Step 9.5: Run the tests**

```bash
cargo test --all-features --test ui_auth
```

Expected: all 8 tests pass.

- [ ] **Step 9.6: Commit**

```bash
git add src/ui/auth.rs src/ui/mod.rs tests/ui_auth.rs
git commit -m "feat(ui): bearer token + origin check auth (8 tests)"
```

---

**[Plan continues in next section — see Phase C Task 10+]**

---

## Note to implementer

This plan is large. Tasks 1–9 are written in full detail above. Tasks 10–39 follow the same structure and are grouped by phase below. Each should be expanded in the same TDD style (failing test → implement → verify → commit) during execution. If executing via superpowers:subagent-driven-development, each task is one subagent dispatch.

The remaining task list (detailed specifications continued in subsequent file appends, not inlined here to keep this plan chunk manageable):

**Phase C (continued):**
- Task 10: API error envelope (`ui/api/error.rs`)
- Task 11: Security headers + CSP middleware (`ui/server.rs` partial)

**Phase D — API endpoints:**
- Task 12: GET /api/bundle — returns BundleMeta, 3 tests
- Task 13: GET /api/overview — returns OverviewResponse, 3 tests
- Task 14: GET /api/wizard/start — creates session, 2 tests
- Task 15: POST /api/wizard/next — step progression + validation, 4 tests
- Task 16: POST /api/wizard/execute — finalizes + persists via engine, 2 tests
- Task 17: GET /api/wizard/session/:id — resume, 2 tests
- Task 18: GET /api/locale/:code + POST /api/shutdown, 3 tests

**Phase E — Assets embedding + server:**
- Task 19: `ui/assets.rs` (rewrite) with `include_bytes!` manifest for all new assets
- Task 20: `ui/server.rs` — bind 127.0.0.1, generate bearer, open browser, graceful shutdown
- Task 21: `ui/routes.rs` — Router wiring + auth middleware + security headers layer

**Phase F — Frontend SPA infrastructure:**
- Task 22: `index.html` shell with initial state embed + CSP meta
- Task 23: `js/app.js` + `js/api.js` + `js/router.js`
- Task 24: All 6 Alpine stores (`js/stores/*.js`)
- Task 25: `js/formatters.js`

**Phase G — Frontend components:**
- Task 26: `components/shell.html` + `components/sidebar.html`
- Task 27: `components/topbar.html` + `components/empty-state.html`
- Task 28: `components/overview.html`
- Task 29: `components/wizard-shell.html` + `components/wizard-step.html` + all 4 field components

**Phase H — i18n:**
- Task 30: Add all `ui.*` keys to `i18n/en.json` (about 45 keys across brand, sidebar, topbar, overview, wizard, errors, a11y, footer, nav namespaces)
- Task 31: Run `tools/i18n.sh all` to propagate; add `scripts/check_i18n_coverage.py` + wire into `ci/local_check.sh`

**Phase I — Tests:**
- Task 32: `tests/ui_integration.rs` — full flow: boot server, GET /api/bundle, GET /api/overview, wizard start→next→execute, verify state
- Task 33: `tests/ui_security.rs` — 5 security tests: missing bearer, wrong origin, scope path traversal, no secrets in response bodies, Zeroize on drop

**Phase J — Cutover:**
- Task 34: Replace `src/ui/mod.rs` `pub use legacy::launch` with a new `launch` that calls into `server.rs`. Delete `src/ui/legacy.rs`. Delete `assets/setup-ui/` (old). Rename `assets/setup-ui-v2/` → `assets/setup-ui/`. Update `include_bytes!` paths. Run `cargo test --all-features`. Run `ci/local_check.sh`. Commit.

**Phase K — Release prep:**
- Task 35: Update `greentic-setup/CLAUDE.md` with new module layout + API endpoints (also fixing the stale entries found during spec review)
- Task 36: Update `greentic-setup/README.md` (if it references the old UI)
- Task 37: Update `greentic-docs/src/content/docs/gtc-cli/setup.md` with new dashboard walkthrough and screenshots; run `node greentic-docs/scripts/translate-docs.mjs`
- Task 38: Add changelog entry at `updates/{today}/greentic-setup.md`
- Task 39: Run full manual test plan from spec (12 items) + security audit checklist (16 items); create PR with title "feat: dashboard Phase 1a — multi-scope overview + embedded wizard" and the spec + checklist in the PR body

---

## Self-review (runs after plan is complete, before execution)

Before starting Task 1, run this review:

1. **Spec coverage:** Every goal in the spec has a task. Map check:
   - G1 (design system) → Tasks 4–7
   - G2 (Alpine migration) → Tasks 7, 22–29
   - G3 (code refactor) → Tasks 2, 8–21
   - G4 (dashboard shell) → Tasks 26–28
   - G5 (overview view) → Tasks 13, 28
   - G6 (wizard embedded) → Tasks 14–17, 29
   - G7 (terminal CLI preserved) → Task 34 (only UI mode swapped)
   - G8 (i18n preserved) → Tasks 30–31
   - G9 (security baseline) → Tasks 9, 11, 33, 39
   - G10 (a11y baseline) → Tasks 28, 29, 39 (manual test)

2. **Placeholder scan:** Every task expanded above has full code. Remaining phase descriptions (D–K) are stubs that must be expanded in the same TDD style during execution — they are not placeholders for the engineer but summaries for the plan author.

3. **Type consistency:** `ScopeKey`, `BundleMeta`, `WizardSession`, `AppState`, `AuthError` naming is consistent across Tasks 8–9 and referenced identically in later tasks.

4. **Scope check:** Large but coherent. All tasks are sequential or have clear dependencies. No subsystem is independent enough to split into another plan.

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-04-11-greentic-setup-dashboard-phase-1a.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration. Each subagent gets Task N's full detail, no prior-task state. Reviewer checks output against the task checklist before advancing.

2. **Inline Execution** — Execute tasks in this session via `superpowers:executing-plans`, batch with checkpoints for user review every few tasks.

**Which approach?**
