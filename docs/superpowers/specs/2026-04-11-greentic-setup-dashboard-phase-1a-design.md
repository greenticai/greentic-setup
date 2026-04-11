# Greentic Setup Dashboard — Phase 1a Design Spec

**Status:** Draft — pending user review
**Date:** 2026-04-11
**Scope:** Phase 1a of multi-phase greentic-setup web UI improvement
**Author:** Brainstormed via superpowers:brainstorming skill

---

## Context

`greentic-setup` is the bundle-configuration tool in the Greentic ecosystem. It ships as **library + CLI binary**: the library is consumed by greentic-operator and other tools; the CLI binary (`src/bin/greentic_setup.rs`) is the direct user-facing entry point.

The binary currently runs in dual-mode: a web UI (Axum + vanilla JS embedded in the binary via `include_str!`) and a terminal fallback (`dialoguer`). A single bundle can host multiple `(tenant, env, team)` configurations; each scope has its own set of configured providers and secrets.

**Note on CLAUDE.md staleness:** At the time this spec was written, `greentic-setup/CLAUDE.md` did not describe `src/ui/`, `src/cli_commands/`, `src/cli_helpers/`, `src/platform_setup/`, `src/engine/`, `src/webhook/`, `src/setup_to_formspec/`, `src/capabilities.rs`, `src/flow.rs`, `src/gtbundle.rs`, or `src/tenant_config.rs` — the documented structure was out of date relative to the actual tree. All statements in this spec about current-state file paths and sizes are based on filesystem inspection, not on CLAUDE.md. Updating CLAUDE.md to reflect the real tree is a documentation task (tracked in the docs checklist below) but is not the goal of this spec.

The existing web UI is a one-shot setup wizard. Users run `greentic-setup <bundle>`, configure one scope, and exit. There is no way to view or switch between configured scopes, no dashboard for ongoing management, and the code has grown unmanageable (`src/ui/mod.rs` = 949 LOC, `assets/setup-ui/app.js` = 42 KB vanilla JS).

The real usage pattern — confirmed during brainstorm — is that operators repeatedly open greentic-setup to manage **multiple scopes of the same bundle**. The current one-shot wizard model does not serve that workflow. We need a dashboard-first UI that treats the setup wizard as one of several views.

This spec covers **Phase 1a** of a phased improvement plan: the foundation and read-only dashboard shell. Later phases (1b, 5, 6) are referenced for context but are out of scope here.

## Phase roadmap

The improvement is decomposed into four phases, each shipping as its own spec → plan → implementation cycle:

| Phase | Scope | Status |
|---|---|---|
| **1a** · Foundation + Read | Design system, Alpine.js migration, code refactor, dashboard shell, scope switcher, overview view (read-only), setup wizard embedded | **This spec** |
| **1b** · Management | Secrets CRUD, provider extension add/remove, capability toggles, rebuild trigger with progress | Future spec |
| **5** · Advanced | Expiry tracking, audit log, rotation policies | Later |
| **6** · Enterprise | RBAC, multi-user, metrics, health checks, OCI registry browser | Later |

Phase 1a alone delivers a usable upgrade: operators can view all configured scopes at a glance, switch between them, and launch the setup wizard to configure new ones. Management actions (editing secrets, adding extensions) still happen through the CLI until Phase 1b lands — but the dashboard foundation is in place and reusable.

## Goals (Phase 1a)

1. **Establish design system.** Design tokens (colors from greentic.ai HSL palette, Poppins typography, Greentic dragon mascot, 4px spacing grid) usable by all future phases and Greentic UIs.
2. **Migrate to Alpine.js.** Replace the 42 KB vanilla JS blob with Alpine.js-based SPA. No build step. All assets embedded in the binary via `include_str!` / `include_bytes!`.
3. **Refactor code structure.** Split monolithic `src/ui/mod.rs` (949 LOC) into focused modules, each ≤ 500 LOC per project coding standards.
4. **Dashboard shell.** Persistent sidebar with brand header, scope switcher (tenant/env/team dropdowns), nav items, status footer. Main area with top bar (breadcrumb, controls) and content region.
5. **Overview view.** Multi-scope landing page showing all configured `(tenant, env, team)` combinations, per-scope provider status, aggregated stats, and warnings. Read-only in Phase 1a.
6. **Setup wizard embedded.** Launchable from dashboard via "Add tenant/env" or "Add provider" actions. Runs inside the dashboard shell, not as a standalone tool.
7. **Preserve terminal CLI.** `--no-ui` fallback mode unchanged — existing `dialoguer` flow stays intact.
8. **Preserve i18n.** All 50+ existing locale bundles continue to work, including RTL. Every new user-visible string routes through the i18n catalog.
9. **Security baseline.** UI handles secrets — apply defense-in-depth network binding, bearer token auth, origin check, no secret leaks in logs/URLs/storage. See Security section.
10. **Accessibility baseline.** WCAG 2.1 AA: keyboard navigation, focus management, ARIA labels, color contrast, RTL support, reduced-motion.

## Non-goals (Phase 1a — deferred to 1b or later)

- Secrets CRUD (list with reveal, edit, rotate, add, delete) — Phase 1b
- Provider extension management (add/remove OCI pack references) — Phase 1b
- Capability toggles (enable/disable bundle feature flags) — Phase 1b
- Rebuild trigger + SSE progress stream — Phase 1b
- Secret rotation policies, expiry tracking, audit log — Phase 5+
- RBAC, multi-user support, health checks, usage metrics — Phase 6+
- OCI registry browser (add extensions via paste URI only in 1b) — later
- Multi-tenant tree nav with search (flat list for now) — later
- Light mode (dark mode only in 1a; light can be added later)
- Modern SPA build step (Vite, React, etc.) — explicitly rejected to keep Rust-only toolchain

## Success criteria

- `cargo build --workspace --all-features` passes
- `cargo test` passes, including new integration + security tests
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean
- `cargo fmt --all -- --check` clean
- All files in `src/ui/**/*.rs` ≤ 500 LOC
- Manual test: run `greentic-setup <bundle>` → dashboard loads → overview shows configured scopes → click "Add tenant / environment" → wizard completes → new scope appears in overview
- Bundle size: frontend assets ≤ 200 KB total
- Existing `i18n/*.json` catalogs work — no regressions in locale display
- Pre-release audit checklist (see Security section) passes

## Target user

Multiple personas with progressive disclosure:

- **Developer** (familiar with Greentic internals) — expects efficiency, advanced mode for dense info
- **DevOps / Platform Engineer** (first-time Greentic user) — expects clarity, sensible defaults
- **Non-technical operator** (end-user running deployments) — expects friendly onboarding, no jargon

The default UI optimizes for the non-technical operator. An "Advanced mode" toggle (in the top bar) unlocks denser information layout, raw JSON preview of bundle state, and keyboard shortcuts. Phase 1a ships the toggle but the advanced overlays can be minimal.

## Visual direction — Warm Wizard × Clean & Minimalist

Brand personality is friendly, grounded by the Greentic dragon mascot and Poppins typography. Execution is clean and minimalist: flat surfaces, generous whitespace, teal accent used sparingly (active states, primary CTA), no decorative glow or gradient noise.

**Warmth comes from:**
- Dragon mascot in sidebar brand header (used once, not repeated)
- Poppins font family (rounded, geometric terminals)
- Friendly copy ("Welcome back", not "Dashboard v2.3")
- Teal brand color for accents

**Minimalism enforced by:**
- One accent color — teal for active states and primary CTA only
- Flat surfaces, 1px borders for elevation (no box shadows by default)
- Generous whitespace (32px content padding, 28–40px between sections)
- Solid buttons (no gradients)
- Stats as text with vertical rule separators, not cards
- Provider chips: outline-only with small dot indicator, no background fill

A well-designed Phase 1a layout should still feel clean and usable if the mascot and teal accents were stripped. Warmth is an additive layer, not a substitute for clear hierarchy.

## Architecture overview

```
greentic-setup <bundle>
    ↓
Axum server on 127.0.0.1:{random_port}
    ↓
Browser opens → SPA shell loads (Alpine.js)
    ↓
Alpine reads initial state (bundle meta, scopes, locale, bearer token)
    ↓
Client-side router handles navigation:
    ├─ /               → Dashboard overview (default)
    ├─ /wizard/new     → Wizard for new scope
    └─ /wizard/provider → Wizard for adding provider to existing scope
    ↓
User changes scope → $store.scope updates → overview refetches → re-render
User clicks "Add" → router navigates → wizard component mounts → POST answers → engine persists → router returns to overview
```

**Key architectural principles:**

1. **Single SPA, multiple views** — Dashboard, overview, wizard all in one page. Client-side routing via Alpine. No page reloads.
2. **Server is thin.** It serves JSON data and static assets. No HTML fragment rendering (no htmx-style). All UI logic lives in Alpine.
3. **State stays server-authoritative.** Alpine stores are views of server state. Every mutation round-trips to the server (via the existing `engine/` + `qa/` modules).
4. **No write in Phase 1a (except wizard submit).** Overview is strictly read-only. Secrets CRUD, extension management, capability toggles all return 404 or are absent from the API entirely — they show up in Phase 1b.
5. **Backward-compat at the engine boundary.** `engine/`, `qa/`, `cli_helpers/`, `qa_persist.rs` are called with the same signatures — only the HTTP layer is replaced.
6. **Full rewrite of UI layer.** No old-vs-new toggle. No feature flag. Old `src/ui/mod.rs` and `assets/setup-ui/*` are deleted outright.

## Code structure

### Backend layout

```
greentic-setup/
├── src/
│   ├── bin/greentic_setup.rs    (unchanged)
│   ├── ui/
│   │   ├── mod.rs                ≤ 80 LOC   public API, re-exports
│   │   ├── server.rs             ≤ 200 LOC  Axum app, bind, browser opener
│   │   ├── routes.rs             ≤ 150 LOC  Router + middleware wiring
│   │   ├── assets.rs             ≤ 150 LOC  Embedded static assets + MIME
│   │   ├── state.rs              ≤ 200 LOC  AppState, DTOs (BundleMeta, ScopeKey, OverviewSummary, WizardSession, WarningMessage)
│   │   ├── auth.rs               ≤ 150 LOC  Bearer token middleware, Origin check
│   │   └── api/
│   │       ├── mod.rs            ≤ 50 LOC   Facade
│   │       ├── bundle.rs         ≤ 200 LOC  GET /api/bundle
│   │       ├── overview.rs       ≤ 250 LOC  GET /api/overview
│   │       ├── wizard.rs         ≤ 300 LOC  wizard start / next / execute / session
│   │       ├── locale.rs         ≤ 100 LOC  GET /api/locale/:code
│   │       └── error.rs          ≤ 80 LOC   ApiError → JSON envelope
│   ├── qa/                       (unchanged)
│   ├── engine/                   (unchanged)
│   ├── cli_commands/             (unchanged)
│   └── ...
```

Total new Rust LOC: approximately 1760, replacing the 949 LOC monolith. The increase is justified by clear module boundaries, the new auth layer, and the split API surface.

**File size is non-negotiable.** Any `src/ui/**/*.rs` file exceeding 500 LOC is a review blocker and must be split before merge.

### Frontend layout

```
assets/setup-ui/
├── index.html                    ≤ 8 KB    SPA shell, initial state embed, CSP meta
├── vendor/
│   ├── alpine.min.js             ~15 KB    Alpine v3, vendored (no CDN)
│   └── poppins/
│       ├── poppins-400.woff2     ~10 KB    Latin subset, self-hosted
│       ├── poppins-500.woff2     ~10 KB
│       ├── poppins-600.woff2     ~10 KB
│       └── poppins-700.woff2     ~10 KB
├── js/
│   ├── app.js                    ≤ 5 KB    Alpine init + boot
│   ├── router.js                 ≤ 3 KB    Client-side routing (pushState)
│   ├── api.js                    ≤ 3 KB    Fetch helpers with bearer token
│   ├── stores/
│   │   ├── bundle.js             ≤ 2 KB    $store.bundle
│   │   ├── scope.js              ≤ 2 KB    $store.scope (tenant/env/team)
│   │   ├── overview.js           ≤ 3 KB    $store.overview
│   │   ├── wizard.js             ≤ 4 KB    $store.wizard
│   │   ├── locale.js             ≤ 2 KB    $store.locale (current code + strings)
│   │   └── ui.js                 ≤ 1 KB    $store.ui (advanced mode, focus)
│   └── formatters.js             ≤ 2 KB    Display formatters (counts, durations, mask)
├── components/                               Alpine templates loaded into DOM
│   ├── shell.html                 shell wrapper (sidebar + main)
│   ├── sidebar.html               brand, scope switcher, nav, footer
│   ├── topbar.html                breadcrumb, locale, advanced, apply button
│   ├── overview.html              welcome, stats, configured scopes list, empty states
│   ├── wizard-shell.html          wizard container (step sidebar, back/next)
│   ├── wizard-step.html           form renderer for FormSpec
│   ├── field-text.html            text / password input
│   ├── field-textarea.html        multi-line input
│   ├── field-select.html          select dropdown
│   ├── field-switch.html          boolean switch
│   └── empty-state.html           shared empty state partial
├── styles/
│   ├── tokens.css                 CSS custom properties (from greentic.ai HSL palette)
│   ├── base.css                   reset, @font-face Poppins, body defaults
│   ├── layout.css                 shell grid, sidebar, topbar, content regions
│   ├── components.css             buttons, inputs, cards, chips, dropdowns
│   └── animations.css             transitions, reduced-motion fallback
└── icons/
    ├── greentic-mascot.png        ~63 KB    Full dragon mascot
    ├── greentic-mascot-sm.png     ~8 KB     30×30 sidebar variant (optional downscale)
    └── *.svg                                Inline-ready icons (plus, check, chevron, etc.)
```

### Bundle size budget

| Category | Size |
|---|---|
| Poppins 4 weights (woff2 Latin subset) | ~40 KB |
| Mascot PNG + icons | ~75 KB |
| Alpine.js v3 (vendored) | ~15 KB |
| Custom JS (app, stores, api, router, formatters) | ~25 KB |
| CSS (tokens, base, layout, components, animations) | ~14 KB |
| HTML shell + component templates | ~10 KB |
| **Total** | **~179 KB** |

The target cap is 200 KB for headroom. This is larger than the existing 42 KB vanilla bundle, but the justification is Poppins brand consistency, reactive Alpine framework, dashboard capability, and mascot personality — all durable foundations for later phases.

### What gets deleted

- `src/ui/mod.rs` (949 LOC) → replaced by split modules
- `assets/setup-ui/app.js` (42 KB) → replaced by Alpine SPA
- `assets/setup-ui/style.css` → replaced by `styles/*.css`
- Existing `assets/setup-ui/index.html` → replaced with new shell

`src/bin/greentic_setup.rs` is unchanged — it already routes to `ui::run_ui_mode()`, and that entry point stays.

## Design tokens

All tokens derived from [greentic.ai](https://greentic.ai) CSS variables (shadcn/ui HSL format) to ensure brand consistency across the Greentic ecosystem.

### Colors (dark mode, the only mode in Phase 1a)

```css
/* Brand */
--brand: hsl(166 70% 45%);           /* Phase 1a default — primary teal */
--brand-hover: hsl(166 70% 55%);     /* Hover state */
--brand-muted: hsl(166 50% 40%);
--brand-bg: hsl(166 70% 45% / 0.1);  /* Active state background */
--brand-border: hsl(166 70% 45% / 0.3);
--brand-ink: hsl(220 20% 8%);        /* Text on brand fill */
--accent: hsl(186 70% 50%);          /* Reserved for hero gradient, if used */

/* Surfaces */
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
```

### Typography

```css
--font-sans: 'Poppins', ui-sans-serif, system-ui, sans-serif;
--font-mono: ui-monospace, SFMono-Regular, Menlo, Monaco, monospace;
```

Poppins weights shipped: 400 (body), 500 (emphasis), 600 (titles), 700 (reserved for display titles only).

Type scale:

- `display` — 28 / 32 / 600
- `title` — 22 / 28 / 600
- `heading` — 16 / 22 / 600
- `body` — 14 / 22 / 400
- `body-sm` — 13 / 20 / 400
- `caption` — 11 / 16 / 500
- `mono` — 12 / 18 / 500

### Spacing (4px base grid)

```
--space-1: 4px   --space-2: 8px   --space-3: 12px  --space-4: 16px
--space-6: 24px  --space-8: 32px  --space-12: 48px --space-16: 64px
```

### Radius

```
--r-sm: 6px    --r-md: 8px    --r-lg: 12px    --r-full: 9999px
```

### Shadows

Only one shadow token exists, used very sparingly:

```
--shadow-subtle: 0 1px 2px hsl(0 0% 0% / 0.2);
```

Elevation is conveyed by 1px borders, not box shadows. Glows and gradients are avoided by default.

### Motion

```
--ease-fast:   120ms cubic-bezier(0.4, 0, 0.2, 1);
--ease-base:   200ms cubic-bezier(0.4, 0, 0.2, 1);
--ease-smooth: 300ms cubic-bezier(0.4, 0, 0.2, 1);
```

Wrapped in `@media (prefers-reduced-motion: reduce)` → durations collapse to 0.01ms.

## Wizard & dashboard layout

### Sidebar (240 px wide)

Vertical regions top → bottom:

1. **Brand header** — Dragon mascot (30×30) + "Greentic Setup" title + bundle display name subtitle.
2. **Scope box** — "Scope" label, then three dropdowns (tenant / env / team) stacked vertically. Active dropdown has teal border + subtle brand-bg fill.
3. **Nav** — "Manage" label, then nav items:
   - Overview (active in Phase 1a)
   - Providers (disabled, tagged "1b")
   - Secrets (disabled, tagged "1b")
   - Capabilities (disabled, tagged "1b")
4. **Footer** — Status dot + "Server running · port {port}" (i18n key: `ui.footer.server_running`).

Disabled Phase 1b nav items remain visible to communicate roadmap. Clicking shows a tooltip (`ui.nav.coming_in_phase_1b`).

### Top bar

Fixed height (~52 px), horizontal:

- **Left:** breadcrumb (e.g., "Overview" or "Overview › Configure demo / prod") in `h1`-like weight
- **Right:** locale selector chip ("ID" / "EN" / etc.), Advanced mode toggle, Apply changes button (disabled in Phase 1a — always "No pending changes")

### Main content — overview view

- **Welcome header:** title + subtitle (e.g., "2 scopes configured · 1 needs attention") and a primary CTA "Add tenant" on the right
- **Stats row:** 4 stats (Scopes / Providers / Secrets / Warnings) as text separated by vertical rules, flanked by top + bottom borders. No boxed cards.
- **"Configured scopes" list:** One card per configured `(tenant, env, team)`. Active scope is highlighted with teal border. Each card shows the scope label, tenant=/env= path, and provider chips.
- **Provider chips:** outline with a small colored dot (green/yellow). The "+ Add provider" chip is dashed, acts as a link to the wizard.
- **Empty states:** When a potential scope is not yet configured, show a dashed border row with the scope name and a "Configure this scope →" link.

### Wizard embedded view

Launched via either `/wizard/new?scope=...` (configure all providers for a new scope) or `/wizard/provider?scope=...&provider=...` (add one provider to existing scope).

Layout when wizard is active:

- **Sidebar** — unchanged shell, but the scope dropdowns show the scope being configured (locked) and the nav item highlights "Overview" (not wizard).
- **Top bar** — breadcrumb reflects wizard: "Overview › Configure demo / prod".
- **Main content** — replaced with a wizard-overlay container that has its own inner sidebar (step list) and main area (form for current step). "Back to Overview" button returns to dashboard.

The wizard reuses the existing `qa/` FormSpec logic via the backend API — only the rendering layer is new.

## API contract

All endpoints under `/api/*` require:

- `Authorization: Bearer <token>` header where the token is generated at server startup and embedded in the initial HTML
- Valid `Origin` or `Referer` header matching `http://127.0.0.1:{port}` or `http://localhost:{port}`
- Standard security headers on responses (`Cache-Control: no-store`, `X-Content-Type-Options: nosniff`, etc.)

### Endpoints

```
GET  /                              SPA shell (embeds initial state + bearer token)
GET  /vendor/...                    Static: Alpine, Poppins
GET  /js/..., /styles/..., /icons/... Static: app assets
GET  /api/bundle                    Bundle metadata + discovered scopes
GET  /api/overview?tenant&env&team  Overview summary + full scope list
GET  /api/wizard/start?tenant&env&team[&provider]
                                    Initialize wizard session
POST /api/wizard/next               Submit current step, receive next
POST /api/wizard/execute            Finalize wizard, persist answers
GET  /api/wizard/session/:id        Get current wizard session state
GET  /api/locale                    List of available locale codes
GET  /api/locale/:code              Locale catalog (JSON strings)
POST /api/shutdown                  Graceful shutdown (UI Exit button)
```

### Key DTOs (shared in `src/ui/state.rs`)

```rust
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

pub struct ScopeKey {
    pub tenant: String,
    pub env: String,
    pub team: String,
}

pub struct ScopeSummary {
    pub scope: ScopeKey,
    pub status: ScopeStatus,      // configured | partial | not_configured
    pub providers: Vec<ProviderStatus>,
    pub warnings: Vec<WarningMessage>,
}

pub struct ProviderStatus {
    pub id: String,
    pub display_name: String,
    pub configured: bool,
    pub secrets_count: u32,       // count only, never values
    pub warnings: Vec<WarningMessage>,
}

pub struct WarningMessage {
    pub key: String,              // i18n key
    pub params: serde_json::Value,
    pub severity: WarningSeverity,
}

pub struct OverviewResponse {
    pub scope: ScopeKey,
    pub stats: OverviewStats,
    pub scopes: Vec<ScopeSummary>,
}

pub struct WizardSession {
    pub id: Uuid,                 // v4 random
    pub scope: ScopeKey,
    pub provider: Option<String>,
    pub current_step: u32,
    pub total_steps: u32,
    pub step: WizardStep,
    pub answers_so_far: serde_json::Value,  // zeroized on drop
}
```

### Error envelope

All errors use a consistent shape:

```json
{
  "error": {
    "code": "bundle.not_found",
    "key": "ui.error.bundle_not_found",
    "params": { "path": "..." },
    "fields": {
      "field_name": { "key": "ui.error.field_invalid", "params": {} }
    }
  }
}
```

- `code` — stable machine code for programmatic handling
- `key` — i18n key for user-facing display
- `params` — ICU parameters
- `fields` — present only on validation errors

HTTP status codes: 400 (validation), 401 (missing/bad bearer), 403 (bad origin), 404 (not found), 409 (conflict), 500 (internal).

### What does not exist in Phase 1a

The following endpoints are explicitly absent. Attempts to call them return 404:

- `/api/secrets/*` — Phase 1b
- `/api/providers/:id/extension` — Phase 1b
- `/api/capabilities/*` — Phase 1b
- `/api/rebuild` — Phase 1b
- WebSocket endpoints — not needed in 1a

## Security

greentic-setup handles secrets. Security is not optional. This section applies defense-in-depth across network, transport, storage, rendering, and logging layers. Implementation must satisfy every item here before release.

### Threat model

**Assets:** Secret values (API tokens, signing secrets, passwords), scope isolation, wizard session state.

**Local-machine threats in scope:**
- Other user on the same machine
- Malicious processes run by the same user (browser extensions, local apps)
- Malicious websites opened in the same browser (CSRF via localhost)
- Browser leaks (history, cache, storage, referrer)
- System logs and crash dumps

**Out of scope for Phase 1a:**
- Remote network attackers (127.0.0.1 only)
- Physical access (disk encryption is the OS's concern)
- Supply chain (dependencies audited separately)

### Network controls

| Control | Implementation |
|---|---|
| Bind loopback only | Axum listener on `127.0.0.1:0`, never `0.0.0.0` |
| Random port | OS-assigned high port (49152+) |
| Auto-shutdown on idle | Tokio timer: exit after 10 minutes with no request |
| Auto-shutdown on client disconnect | Heartbeat ping from browser; exit if no reconnect |
| Basic rate limiting | Max 100 req/min per endpoint |

### Authentication — bearer token + origin check

A cryptographically random 256-bit token is generated at server startup. It is embedded in the initial HTML (inside a `<script id="initial-state">` tag, not a query string), so Alpine can pick it up on boot and attach it to every API request.

```rust
fn require_auth(headers: &HeaderMap, state: &AppState) -> Result<(), StatusCode> {
    let provided = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if !constant_time_eq(provided.as_bytes(), state.bearer_token.as_bytes()) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let origin = headers.get("origin").and_then(|h| h.to_str().ok()).unwrap_or("");
    let expected_127 = format!("http://127.0.0.1:{}", state.port);
    let expected_local = format!("http://localhost:{}", state.port);
    if origin != expected_127 && origin != expected_local {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(())
}
```

Even on 127.0.0.1, bearer + origin checks are mandatory to defend against:
- Other tabs or extensions in the same browser making XHR to the server
- Local apps that cannot read same-origin DOM but can send HTTP requests

### Secret handling

**Never return raw secret values in API responses.** Phase 1a does not have a reveal endpoint, but DTOs are shaped to make a leak a type error:

```rust
pub struct ProviderStatus {
    pub id: String,
    pub configured: bool,
    pub secrets_count: u32,
    // There is deliberately no `secret_values` field.
}
```

**POST bodies, never GET query strings.** Wizard answers submitted via POST body. Query parameters carrying secrets are forbidden and would land in access logs, browser history, and Referer headers.

**In-memory wrappers.** Wizard answers held in `Zeroizing<HashMap<String, String>>` so the memory is scrubbed on drop. `WizardSession` derives `Zeroize` and uses `#[zeroize(drop)]`.

### Logging discipline

No secrets in any log line. Enforced by convention and by scrub audit:

```rust
// Wrong — would leak secrets:
// tracing::debug!(answers = ?answers, "submitted");

// Right:
tracing::info!(
    session_id = %session_id,
    field_count = answers.len(),
    "wizard step submitted",
);
```

Request logging middleware excludes request bodies for `/api/wizard/*` and `/api/secrets/*` (when they exist in 1b).

### Browser-side defenses

**Secret input elements:**

```html
<input
  type="password"
  autocomplete="off"
  spellcheck="false"
  x-model="$store.wizard.answers[field.name]"
>
```

**No persistent client storage for wizard answers.** Use Alpine stores (in-memory only). No `localStorage`, `sessionStorage`, or `IndexedDB` writes for secret-bearing data.

**Response headers on `/api/*`:**

```
Cache-Control: no-store, no-cache, must-revalidate
Pragma: no-cache
X-Content-Type-Options: nosniff
X-Frame-Options: DENY
Referrer-Policy: no-referrer
```

**Initial HTML CSP:**

```html
<meta name="referrer" content="no-referrer">
<meta http-equiv="Content-Security-Policy" content="
  default-src 'self';
  script-src 'self';
  style-src 'self' 'unsafe-inline';
  img-src 'self' data:;
  font-src 'self';
  connect-src 'self';
  frame-ancestors 'none';
  base-uri 'none';
  form-action 'none';
">
```

### Input validation

All inputs from the browser are untrusted, even on localhost.

```rust
pub fn validate_scope(scope: &ScopeKey, bundle: &BundleMeta) -> Result<(), ApiError> {
    if !bundle.available_tenants.contains(&scope.tenant) {
        return Err(ApiError::new("scope.invalid_tenant", "ui.error.invalid_tenant"));
    }
    // ... env, team similar ...
    for part in [&scope.tenant, &scope.env, &scope.team] {
        if part.contains("..") || part.contains('/') || part.contains('\\') {
            return Err(ApiError::new("scope.path_traversal", "ui.error.scope_invalid"));
        }
    }
    Ok(())
}
```

Length limits:
- Tenant / env / team names: ≤ 64 chars
- Provider IDs: ≤ 128 chars
- Secret values: ≤ 8192 chars (tokens can be long)
- Generic text fields: ≤ 1024 chars

### Error messages

Server-side logs get detailed error context. Browser-visible responses get a generic i18n key and never reveal file paths, stack traces, or internal state.

```rust
// Wrong:
// Err(anyhow!("Failed to read /home/user/.config/secrets/slack.yaml"))

// Right:
tracing::error!(path = ?secret_path, error = %e, "failed to read secret");
Err(ApiError::new("secrets.read_failed", "ui.error.secrets_read_failed"))
```

### Session management

Wizard sessions are stored in-memory only. Session IDs are v4 UUIDs (cryptographically random). Sessions expire after 30 minutes of inactivity. A cleanup task runs every 5 minutes to drop expired sessions. Server shutdown clears all session state. Nothing is persisted to disk by the session layer.

### File system

Secret persistence goes through the existing `greentic-secrets` crate — no new file-writing primitives. Permissions enforced: 0600 for secret files, 0644 for non-sensitive config. Atomic writes (temp + rename). No temporary files with raw secret content.

### Process boundaries

Secrets must not escape the greentic-setup process via OS primitives:
- Never pass secrets via environment variables to child processes (visible via `/proc/PID/environ` to same-user attackers)
- Never pass secrets via command-line arguments (visible via `ps`)
- The browser subprocess (launched via `xdg-open` / `open` / `start`) receives only the URL, not any secret material

### Pre-release audit checklist

Every release must pass this checklist. Each item is a release blocker:

1. `grep -rn 'tracing::\|println!\|dbg!' src/ui/ | grep -iE 'secret|token|password|answer'` returns empty
2. Axum listener bind line is literally `127.0.0.1:0`, not `0.0.0.0:0`
3. Bearer token generated at startup, embedded in HTML, required on all `/api/*`
4. Origin header check active on all `/api/*`
5. CSP meta tag present in `index.html`
6. `Cache-Control: no-store` applied to all `/api/*` responses
7. Every secret input field has `type="password" autocomplete="off" spellcheck="false"`
8. No `localStorage` / `sessionStorage` writes for wizard answers (grep confirms)
9. `Zeroize` derived on `WizardSession`, test confirms answers zeroed on drop
10. Scope parameters validated against bundle.yaml allow-list (test confirms path traversal rejected)
11. Error responses use i18n keys, no file paths / stack traces leaked (integration test)
12. `curl` without bearer token → 401
13. `curl` with wrong `Origin` → 403
14. Browser devtools show no secrets in `localStorage`, `sessionStorage`, URL, or history
15. Server auto-shutdown after 10 min idle verified
16. tracing output contains no secret values (ran through grep)

## Accessibility (WCAG 2.1 AA)

**Keyboard navigation.** Every interactive element reachable via Tab in logical reading order (sidebar → topbar → content). Focus ring visible via `:focus-visible` with brand color, 2px. Scope switcher fully keyboard-operable (Space / Enter to open, arrow keys to navigate, Esc to close). Wizard form uses native Tab order. Enter on the primary button submits. Esc closes overlays.

**Screen reader support.** Semantic HTML (`<nav>`, `<main>`, `<aside>`, proper heading hierarchy, `<button>` not `<div onclick>`). ARIA labels on icon-only buttons. `aria-live="polite"` on stats and status regions. `aria-current="page"` on active nav. `aria-expanded` on dropdowns. `role="alert"` on error messages. Form fields use `<label for>` binding.

**Color contrast.** Minimum 4.5:1 on all text. Brand teal on dark bg measures ~7.1:1, muted text ~4.8:1. Warning and danger tones verified against WCAG AA. Color is never the only indicator — always paired with an icon or text label (e.g., "⚠ Missing token", not bare red text).

**Reduced motion.** `@media (prefers-reduced-motion: reduce)` collapses all animations and transitions to 0.01ms. All transitions are decorative — the UI functions identically when disabled.

**RTL support.** Use CSS logical properties (`margin-inline-start`, `padding-block-end`, `inset-inline`) instead of directional ones. Sidebar position flips to the right in RTL locales automatically. Directional icons (`←`, `→`) swap. `dir="auto"` on input fields. Tested locales: ar, he, fa, ur.

**Focus management on navigation.** When routing (e.g., Overview → Wizard), focus moves to the new view's h1. When closing a dropdown, focus returns to the trigger.

## i18n

### Key namespace

```
ui.brand.*        Product name, subtitles
ui.sidebar.*      Nav items, labels, scope box
ui.topbar.*       Breadcrumb fallback, controls
ui.overview.*     Welcome, stats, scope cards, empty states
ui.wizard.*       Wizard flow (steps, fields, buttons, review)
ui.errors.*       Error messages (generic + field validation)
ui.a11y.*         Screen reader labels
ui.footer.*       Status strip
ui.nav.*          Nav metadata (coming-soon tooltips, etc.)
```

### ICU message format

```json
{
  "ui.overview.welcome_summary": "{count, plural, =0 {No scopes yet} one {# scope configured} other {# scopes configured}} · {warnings, plural, =0 {all good} one {# needs attention} other {# need attention}}"
}
```

### Loading

1. Initial HTML embeds the current locale's catalog as `<script id="initial-strings">{ ... }</script>`
2. Alpine reads it into `$store.locale.strings` on boot
3. Template helper `t(key, params = {})` does ICU format lookup
4. Locale switch fetches `GET /api/locale/:code`, replaces `$store.locale.strings`, Alpine re-renders reactively

Fallback chain: requested locale → `en` → key itself (`ui.overview.welcome_title` appears verbatim in dev if a key is missing, making bugs obvious).

### RTL detection

```js
const RTL_LOCALES = ['ar', 'he', 'fa', 'ur'];
document.documentElement.dir = RTL_LOCALES.includes(locale) ? 'rtl' : 'ltr';
document.documentElement.lang = locale;
```

### Auto-translation pipeline

When new keys are added to `en.json`, run `greentic-i18n-translator` to propagate to the other 50+ locales. Manual review is recommended for any user-facing copy.

### 100 % coverage enforcement

A CI check (`scripts/check_i18n_coverage.py`) walks `assets/setup-ui/**/*.html` and `js/*.js` and fails the build if:
- Any quoted English literal appears outside a `t(...)` call
- Any `t("key")` references a key missing from `en.json`

### Brand name rules

User-visible copy uses the friendly product name "Greentic Setup". Never the hyphenated crate name `greentic-setup` (which stays in `Cargo.toml`, CLI binary paths, imports, and log lines). Rule enforced by memory reference `feedback_ui_copy_and_i18n.md`.

### Code language rules

Per user preference: all code, tracing messages, error strings, comments, doc comments, README files, and test names are English. Bahasa Indonesia applies only to chat conversation with the developer and to interactive end-user CLI output shown to non-developers — which is the same text in `i18n/id.json`, not hardcoded.

## Testing

### Unit tests (Rust)

- `src/ui/api/bundle.rs` — GET /api/bundle response shape, scope discovery, provider listing
- `src/ui/api/overview.rs` — overview stats calculation, warning aggregation, multi-scope list
- `src/ui/api/wizard.rs` — session lifecycle, step progression, submission validation, finalize
- `src/ui/state.rs` — DTO serialization stability, ScopeKey validation, path traversal rejection
- `src/ui/api/error.rs` — error envelope shape
- `src/ui/auth.rs` — bearer token comparison, origin parsing

### Integration tests

`tests/ui_integration.rs` spawns a real Axum server on a random port, uses `reqwest` to drive it end-to-end:

- Full flow: GET /api/bundle → GET /api/overview → GET /api/wizard/start → POST /api/wizard/next (each step) → POST /api/wizard/execute → GET /api/overview (verify new scope present)
- Auth failures: 401 without bearer, 403 with wrong Origin, 403 with no Origin
- Validation: invalid scope returns 400 with correct error key + path traversal rejected
- Security: inspect all response bodies, assert no field named `secret_value` or `token_value` or similar leaks

### Security smoke tests

`tests/security.rs`:

```rust
#[tokio::test]
async fn rejects_missing_bearer_token() { /* ... */ }

#[tokio::test]
async fn rejects_wrong_origin() { /* ... */ }

#[tokio::test]
async fn scope_validation_rejects_path_traversal() { /* ... */ }

#[tokio::test]
async fn api_responses_never_contain_secret_values() { /* ... */ }

#[tokio::test]
async fn wizard_answers_zeroized_on_drop() { /* ... */ }
```

### Frontend tests — manual for Phase 1a

No formal JS test framework is added (explicit out-of-scope to avoid build-step creep). Manual test plan documented here as the release gate:

1. Run `greentic-setup <demo-bundle>` → dashboard loads, mascot visible in sidebar, brand reads "Greentic Setup"
2. Scope dropdowns open on click, keyboard navigable (arrow keys, Enter, Esc)
3. Switch tenant → overview refetches, stats and scope list update
4. Click "Add tenant / environment" → wizard opens, breadcrumb updates
5. Complete wizard end to end → returns to overview with new scope in list
6. Keyboard-only navigation → all controls reachable via Tab / Shift-Tab / Enter / Esc
7. Screen reader pass (NVDA / JAWS / Orca) → all regions and controls announced
8. Locale switch (en → id → ar) → strings update, RTL flips correctly
9. `prefers-reduced-motion` → transitions disabled, UI still fully functional
10. DevTools inspection → no secrets in localStorage, sessionStorage, URL bar, or history
11. DevTools network tab → Cache-Control: no-store on all /api/* responses
12. Close browser tab → server auto-shutdown within the heartbeat window

Frontend test automation (Playwright / Vitest) is deferred to a later phase. The manual test plan is the Phase 1a gate.

## Migration & rollout

**Branching.** New branch `feat/dashboard-phase-1a` off `main`. Single branch, single PR.

**Atomic rewrite.** Delete old `src/ui/mod.rs` and `assets/setup-ui/*` in the same commit that introduces the new modules and assets. No parallel old/new paths, no feature flag, no `--legacy-ui` toggle.

**Backward compatibility.** CLI flags `--ui`, `--no-ui`, `--answers`, `--locale`, `--bundle` at the `bin/greentic_setup.rs` level are preserved. `--no-ui` terminal mode flows through the unchanged `dialoguer` path. Only the web UI HTTP layer is replaced.

**API breaking changes.** Existing endpoints `/api/providers`, `/api/scope`, `/api/execute` are removed and replaced with the new endpoints documented above. Confirmed from codebase exploration that nothing outside `greentic-setup` itself calls these endpoints — no external scripts, automations, or CI jobs depend on the shape.

**Rollback plan.** If a blocking issue is found post-merge, revert the merge commit. Old UI is recoverable only through git history; there is no runtime rollback.

**Release sequence.**

1. Merge `feat/dashboard-phase-1a` to `main`
2. Bump `greentic-setup` version in `Cargo.toml` — minor bump (e.g. 0.6.x → 0.7.0) because the web UI is fully rewritten, API shape changed
3. Update `greentic-setup/CLAUDE.md` with the new module structure and endpoint list
4. Update `greentic-setup/README.md` if it contains screenshots or UI descriptions
5. Update `greentic-docs/src/content/docs/gtc-cli/setup.md` with new dashboard flow, screenshots, and API changes
6. Run `node greentic-docs/scripts/translate-docs.mjs` to propagate docs to other locales
7. Record changelog entry at `updates/2026-04-NN/greentic-setup.md`
8. Publish to crates.io + GHCR via the existing CI pipeline

**Docs update checklist (required before release):**

- [ ] `greentic-setup/CLAUDE.md` — new module structure and API surface described
- [ ] `greentic-setup/README.md` — updated screenshots or UI flow description if present
- [ ] `greentic-docs/src/content/docs/gtc-cli/setup.md` — dashboard walkthrough, scope switcher, wizard-in-shell explanation
- [ ] `greentic-docs` translations regenerated via `scripts/translate-docs.mjs`
- [ ] Parent `/home/bimbim/works/greentic/CLAUDE.md` — updated only if repo map description changes
- [ ] Changelog entry under `updates/{release-date}/greentic-setup.md`

## Open questions (to resolve during planning, not spec)

- Advanced mode exact content for Phase 1a — minimum is the toggle itself, but what does it unlock? Suggestion: raw JSON preview of overview data and keyboard shortcuts panel. Can be minimal.
- Precise heartbeat / auto-shutdown interval — 10 min is a placeholder, tune based on real usage once shipped.
- Mascot small variant — should we ship a separate 32×32 PNG, or CSS-scale the full 224×200 version? Minor.
- CSP directive tuning — `'unsafe-inline'` on `style-src` currently allowed because Alpine's `x-show`, `x-bind:class` bindings generate inline styles. Can we tighten? Investigate during implementation.

## References

- Greentic ecosystem: `/home/bimbim/works/greentic/CLAUDE.md`
- Project code layout: `/home/bimbim/works/greentic/greentic-setup/`
- Brand assets: https://greentic.ai (HSL color palette extracted from their shadcn/ui tokens)
- Dragon mascot source: `https://greentic.ai/assets/greentic-logo-B2qbsAIc.png`
- Durable feedback:
  - `memory/feedback_ui_copy_and_i18n.md` — UI copy + i18n rules
  - `memory/feedback_ui_style_clean_minimalist.md` — minimalist aesthetic
  - `memory/feedback_security_secrets_handling.md` — security for secrets-handling UIs
  - `memory/feedback_coding_standards.md` — max 500 LOC/file, security-first
