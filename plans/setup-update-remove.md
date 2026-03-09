# Plan: Setup/Update/Remove Refactor

> Refactor setup logic from greentic-operator into greentic-setup, add runtime admin API, and enhance QA capabilities.

## Current State

### What Exists

| Component | Location | Lines | Status |
|-----------|----------|-------|--------|
| Wizard (create/update/remove) | `wizard.rs` | 2,801 | Working, CLI-only |
| QA setup wizard | `qa_setup_wizard.rs` | 612 | Working, CLI + partial HTTP |
| QA persistence (secrets + config) | `qa_persist.rs` | 424 | Working |
| Setup input loader | `setup_input.rs` | 391 | Working |
| Secrets setup (DevStore) | `secrets_setup.rs` | 160 | Working |
| QA bridge (provider → FormSpec) | `demo/qa_bridge.rs` | 356 | Working |
| Onboard HTTP API | `onboard/api.rs` | 145 | Working, unauthenticated |
| HTTP ingress (routing) | `demo/http_ingress.rs` | 1,667 | Working, no admin routes |
| greentic-qa FormSpec engine | `greentic-qa/crates/qa-spec/` | external | Has visible_if, not used yet |
| greentic-provision engine | `greentic-provision/` | external | Not integrated yet |

### What's Missing

- **greentic-setup crate** — all setup logic embedded in greentic-operator
- **Runtime admin API** — no add/upgrade/remove while operator is running
- **mTLS admin endpoint** — no auth on `/api/onboard/*`
- **Full conditional questions** — `visible_if` exists in qa-spec but not wired
- **Hot reload** — must restart operator after bundle changes

---

## Architecture Target

```
                        ┌─────────────────────┐
                        │   greentic-setup     │  ← NEW CRATE
                        │  (library crate)     │
                        ├─────────────────────┤
                        │ SetupEngine          │
                        │ ├─ plan()            │  create/update/remove plans
                        │ ├─ execute()         │  execute plan
                        │ ├─ validate()        │  validate answers
                        │ └─ persist()         │  secrets + config
                        │                      │
                        │ QaEngine             │
                        │ ├─ render_card()     │  adaptive card rendering
                        │ ├─ collect()         │  interactive collection
                        │ └─ conditional()     │  visible_if evaluation
                        │                      │
                        │ BundleManager        │
                        │ ├─ create_bundle()   │
                        │ ├─ update_bundle()   │
                        │ └─ remove_bundle()   │
                        └──────────┬──────────┘
                                   │
                    ┌──────────────┼──────────────┐
                    │              │              │
            ┌───────▼──────┐ ┌────▼─────┐ ┌──────▼──────┐
            │  operator    │ │  runner   │ │  CLI tool   │
            │  (demo mode) │ │  (prod)   │ │  (standalone│
            │              │ │          │ │   setup)    │
            └──────────────┘ └──────────┘ └─────────────┘
```

---

## Phase 1: Extract greentic-setup crate

**Goal:** Move setup/wizard logic into a reusable library crate.

### 1.1 Create crate structure

```
greentic-setup/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── engine.rs          ← SetupEngine (from wizard.rs plan/execute)
│   ├── plan.rs            ← WizardPlan, WizardPlanStep, WizardMode
│   ├── bundle.rs          ← Bundle create/update/remove ops
│   ├── qa/
│   │   ├── mod.rs
│   │   ├── bridge.rs      ← from qa_bridge.rs
│   │   ├── persist.rs     ← from qa_persist.rs
│   │   ├── wizard.rs      ← from qa_setup_wizard.rs
│   │   └── input.rs       ← from setup_input.rs
│   ├── secrets.rs         ← from secrets_setup.rs
│   └── webhook.rs         ← webhook auto-setup (from wizard.rs)
```

### 1.2 Extract modules (in order)

| Step | Source (greentic-operator) | Target (greentic-setup) | Lines | Dependencies |
|------|---------------------------|------------------------|-------|--------------|
| 1 | `setup_input.rs` | `qa/input.rs` | 391 | serde_json, serde_yaml |
| 2 | `secrets_setup.rs` | `secrets.rs` | 160 | greentic-secrets-provider-dev |
| 3 | `qa_persist.rs` | `qa/persist.rs` | 424 | greentic-secrets-provider-dev, DevStore |
| 4 | `qa_bridge.rs` | `qa/bridge.rs` | 356 | greentic-qa (qa-spec) |
| 5 | `qa_setup_wizard.rs` | `qa/wizard.rs` | 612 | qa-spec, qa/bridge, qa/persist |
| 6 | `wizard.rs` (plan/execute) | `engine.rs` + `plan.rs` + `bundle.rs` | ~2,000 | all above |

### 1.3 Keep in greentic-operator (thin wrappers)

- `cli.rs` — CLI argument parsing, delegates to `greentic_setup::SetupEngine`
- `onboard/api.rs` — HTTP handlers, delegates to `greentic_setup::QaEngine`
- `capability_bootstrap.rs` — operator-specific bootstrap logic

### 1.4 Acceptance criteria

- [ ] `cargo test --workspace` passes in both greentic-setup and greentic-operator
- [ ] `demo wizard --execute` works identically
- [ ] `demo setup-wizard` works identically
- [ ] `/api/onboard/qa/*` endpoints work identically
- [ ] No circular dependencies

---

## Phase 2: Full QA conditional questions

**Goal:** Wire greentic-qa's `visible_if` into the setup wizard.

### 2.1 Current qa-spec capabilities (already implemented, not used)

```rust
// greentic-qa/crates/qa-spec/src/spec/question.rs
pub struct QuestionSpec {
    pub visible_if: Option<Expr>,  // ← EXISTS but unused
    pub constraint: Option<Constraint>,
    pub secret: bool,
    pub computed: Option<Expr>,
}
```

```rust
// greentic-qa/crates/qa-spec/src/visibility.rs
pub fn resolve_visibility(spec: &FormSpec, answers: &Value, mode: VisMode) -> VisibilityMap
```

### 2.2 Tasks

| Task | File | Description |
|------|------|-------------|
| 2.2.1 | `qa/bridge.rs` | Map provider QA `visible_if` field to `QuestionSpec.visible_if` |
| 2.2.2 | `qa/wizard.rs` | Call `resolve_visibility()` before rendering each card |
| 2.2.3 | `qa/wizard.rs` | Skip invisible questions in interactive prompt loop |
| 2.2.4 | `qa/wizard.rs` | Skip invisible questions in validation |
| 2.2.5 | `qa/persist.rs` | Don't persist answers for invisible/skipped questions |
| 2.2.6 | Pack QA specs | Add `visible_if` expressions to provider QA JSON files |

### 2.3 Example: conditional question in provider QA

```json
{
  "id": "redis_password",
  "kind": "String",
  "title_i18n": { "key": "state.redis.qa.setup.redis_password" },
  "required": false,
  "secret": true,
  "visible_if": { "field": "redis_auth_enabled", "eq": "true" }
}
```

### 2.4 Secret masking

- `QuestionSpec.secret = true` → render as `Input.Password` in adaptive card
- qa-spec already supports this — just ensure `qa/bridge.rs` sets the flag
- Add `SecretsPolicy` to FormSpec for runtime read/write control

### 2.5 Acceptance criteria

- [ ] Questions with `visible_if` only appear when condition is met
- [ ] Skipped questions don't get persisted
- [ ] Secret questions render as password inputs in adaptive cards
- [ ] Conditional jumps work in both CLI and HTTP onboard API

---

## Phase 3: Admin endpoint (mTLS)

**Goal:** Add secure runtime admin API for bundle lifecycle management.

### 3.1 Admin API design

```
POST   /admin/v1/bundles                    → deploy new bundle
PUT    /admin/v1/bundles/{bundle_id}         → upgrade existing bundle
DELETE /admin/v1/bundles/{bundle_id}         → remove bundle
GET    /admin/v1/bundles                     → list active bundles
GET    /admin/v1/bundles/{bundle_id}/status  → bundle health/status

POST   /admin/v1/setup/qa/spec              → get FormSpec for pack
POST   /admin/v1/setup/qa/validate          → validate answers
POST   /admin/v1/setup/qa/submit            → submit + deploy

POST   /admin/v1/capabilities/{cap_id}/setup → setup capability pack
```

### 3.2 mTLS implementation

```rust
// greentic-setup/src/admin/tls.rs

pub struct AdminTlsConfig {
    /// Server certificate + key
    pub server_cert: PathBuf,
    pub server_key: PathBuf,
    /// CA certificate for client verification
    pub client_ca: PathBuf,
    /// Optional: allowed client CN patterns
    pub allowed_clients: Vec<String>,
}
```

| Task | Description |
|------|-------------|
| 3.2.1 | Add `rustls` + `axum-server` dependencies for TLS |
| 3.2.2 | Implement `AdminTlsConfig` loader (from `greentic.toml` or CLI flags) |
| 3.2.3 | Create mTLS middleware that verifies client certificate |
| 3.2.4 | Bind admin server on separate port (e.g., 8443) |
| 3.2.5 | Add `--admin-port`, `--admin-cert`, `--admin-key`, `--admin-ca` CLI flags |

### 3.3 Admin endpoint in http_ingress.rs

```rust
// Separate Axum router for admin API
fn admin_router(state: AdminState) -> Router {
    Router::new()
        .route("/admin/v1/bundles", post(deploy_bundle))
        .route("/admin/v1/bundles/:id", put(upgrade_bundle))
        .route("/admin/v1/bundles/:id", delete(remove_bundle))
        .route("/admin/v1/bundles", get(list_bundles))
        .route("/admin/v1/setup/qa/spec", post(qa_spec))
        .route("/admin/v1/setup/qa/submit", post(qa_submit))
        .layer(mTlsLayer)
}
```

### 3.4 Acceptance criteria

- [ ] Admin API only accessible with valid client certificate
- [ ] Deploy/upgrade/remove bundles via API
- [ ] Bundle deployment triggers hot-reload (Phase 4)
- [ ] CLI tool `gtc admin deploy --bundle ./my.gtbundle --cert client.pem` works

---

## Phase 4: Hot reload

**Goal:** Apply bundle changes without operator restart.

### 4.1 Current limitations

- `DemoRunnerHost` is created once at startup with `Arc::new()`
- Pack catalog (`HashMap<(Domain, String), ProviderPack>`) is immutable after init
- State store, secrets manager are set once

### 4.2 Hot reload strategy

```
Admin API receives new bundle
    │
    ▼
BundleManager::update_bundle()
    │
    ├─ Validate new bundle (schema, packs, signatures)
    ├─ Diff with current bundle (added/removed/changed packs)
    ├─ Seed new secrets (if needed)
    ├─ Update capability registry
    │
    ▼
RunnerHost::reload()
    │
    ├─ Swap pack catalog (ArcSwap)
    ├─ Reload changed WASM components
    ├─ Update provider routes
    └─ Log reload event
```

### 4.3 Implementation tasks

| Task | Description |
|------|-------------|
| 4.3.1 | Replace `Arc<DemoRunnerHost>` with `Arc<ArcSwap<DemoRunnerHost>>` |
| 4.3.2 | Add `DemoRunnerHost::reload_from_discovery()` method |
| 4.3.3 | Implement `BundleManager::diff()` to compute pack changes |
| 4.3.4 | Wire admin `PUT /bundles/{id}` to trigger reload |
| 4.3.5 | Add graceful drain for in-flight requests during reload |

### 4.4 Acceptance criteria

- [ ] Bundle upgrade via admin API takes effect without restart
- [ ] In-flight requests complete before old packs are dropped
- [ ] New provider routes are registered dynamically
- [ ] Rollback on failed reload

---

## Phase 5: Adaptive Card setup UI (nice to have)

**Goal:** Enable setup/onboard workflow via adaptive cards in messaging channels.

### 5.1 Security considerations

**Options:**
1. **mTLS-gated admin channel** — Setup cards only sent via admin API, rendered in a secure internal channel
2. **Short-lived setup token** — Generate one-time URL with expiring token, e.g., `https://operator/setup?token=abc123`
3. **Role-based access** — Only users with `admin` role (via OAuth) can trigger setup cards
4. **Internal-only tunnel** — Setup endpoint only accessible via internal network/VPN

**Recommendation:** Option 2 (short-lived token) + Option 3 (role-based) combined:
- Admin generates setup URL via CLI: `gtc admin setup-link --bundle ./my.gtbundle --expires 30m`
- URL contains signed JWT with setup scope
- Setup card rendered in WebChat or any channel
- Answers submitted via card → QA engine → persist → deploy

### 5.2 Tasks

| Task | Description |
|------|-------------|
| 5.2.1 | Create setup flow pack that renders QA FormSpec as adaptive cards |
| 5.2.2 | Implement multi-step card wizard (card → submit → next card → done) |
| 5.2.3 | Add signed JWT generation for setup URLs |
| 5.2.4 | Validate JWT on setup card submission |
| 5.2.5 | Handle secret inputs (password masking in cards) |
| 5.2.6 | End-to-end: adaptive card setup → QA engine → persist → deploy |

### 5.3 Acceptance criteria

- [ ] Setup can be completed entirely via adaptive cards
- [ ] Secret values are masked in card UI
- [ ] Conditional questions work (show/hide based on previous answers)
- [ ] Setup link expires after configured time
- [ ] Only authorized users can access setup cards

---

## Implementation Order

```
Phase 1: Extract greentic-setup        ██████████░░░░░░░░░░  ~2 weeks
Phase 2: Conditional questions          ████░░░░░░░░░░░░░░░░  ~1 week
Phase 3: Admin endpoint (mTLS)          ████████░░░░░░░░░░░░  ~1.5 weeks
Phase 4: Hot reload                     ████████████░░░░░░░░  ~2 weeks
Phase 5: Adaptive card setup (N2H)      ██████░░░░░░░░░░░░░░  ~1 week
```

### Dependencies

```
Phase 1 ──→ Phase 2 (uses greentic-setup QaEngine)
Phase 1 ──→ Phase 3 (uses greentic-setup SetupEngine)
Phase 3 ──→ Phase 4 (admin API triggers hot reload)
Phase 2 ──→ Phase 5 (conditional cards need QA engine)
Phase 4 ──→ Phase 5 (card submit triggers deploy + reload)
```

### Risk Areas

| Risk | Impact | Mitigation |
|------|--------|------------|
| wizard.rs too coupled to cli.rs | Phase 1 takes longer | Extract pure logic first, keep CLI wrappers |
| Hot reload race conditions | Data corruption | ArcSwap + drain middleware + rollback |
| mTLS cert management complexity | Deployment friction | Provide cert generation CLI tool |
| Adaptive card security | Unauthorized setup | Short-lived JWT + role check |
| Breaking existing wizard CLI | User disruption | Keep all CLI flags identical, re-export from greentic-setup |

---

## Files to Create/Modify

### New files (greentic-setup crate)

```
greentic-setup/Cargo.toml
greentic-setup/src/lib.rs
greentic-setup/src/engine.rs
greentic-setup/src/plan.rs
greentic-setup/src/bundle.rs
greentic-setup/src/qa/mod.rs
greentic-setup/src/qa/bridge.rs
greentic-setup/src/qa/persist.rs
greentic-setup/src/qa/wizard.rs
greentic-setup/src/qa/input.rs
greentic-setup/src/secrets.rs
greentic-setup/src/webhook.rs
greentic-setup/src/admin/mod.rs       (Phase 3)
greentic-setup/src/admin/tls.rs       (Phase 3)
greentic-setup/src/admin/routes.rs    (Phase 3)
greentic-setup/src/reload.rs          (Phase 4)
```

### Modified files (greentic-operator)

```
Cargo.toml                            → add greentic-setup dependency
src/cli.rs                            → thin wrappers calling greentic-setup
src/onboard/api.rs                    → delegate to greentic-setup::QaEngine
src/demo/http_ingress.rs              → mount admin router (Phase 3)
src/capability_bootstrap.rs           → use greentic-setup::secrets (minor)
```
