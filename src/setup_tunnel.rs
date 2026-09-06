use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::{Map as JsonMap, Value};
use sha2::{Digest, Sha256};

pub struct SetupTunnel {
    pub mode: String,
    pub local_base_url: String,
    pub public_base_url: String,
    /// `None` when reusing a tunnel recorded by another Greentic process
    /// (the shared record owns it, not this setup session).
    child: Option<Child>,
    /// Cloudflared tunnels deliberately OUTLIVE setup so the runtime they
    /// were configured against keeps a live public URL (greentic-start adopts
    /// them via the shared record). ngrok keeps the old kill-on-drop
    /// semantics until it gets the same shared-record treatment.
    kill_on_drop: bool,
}

impl Drop for SetupTunnel {
    fn drop(&mut self) {
        if self.kill_on_drop
            && let Some(child) = self.child.as_mut()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl SetupTunnel {
    pub fn is_running(&mut self) -> bool {
        match self.child.as_mut() {
            Some(child) => child.try_wait().ok().flatten().is_none(),
            // Reused shared-record tunnel: not our child. Liveness is
            // enforced by the URL probes callers already run.
            None => true,
        }
    }

    /// Handle for a tunnel owned elsewhere (the shared record, or tests):
    /// no child process, never killed on drop.
    pub(crate) fn detached(mode: &str, local_base_url: &str, public_base_url: &str) -> Self {
        Self {
            mode: mode.to_string(),
            local_base_url: local_base_url.trim_end_matches('/').to_string(),
            public_base_url: public_base_url.to_string(),
            child: None,
            kill_on_drop: false,
        }
    }
}

/// Default Greentic-operated Worker tunnel base (mirrors greentic-start's
/// `gtunnel::DEFAULT_WORKER_BASE_URL`; the crates don't share a dependency).
const DEFAULT_GTUNNEL_WORKER_BASE_URL: &str = "https://greentic-webhook-proxy.greentic.workers.dev";

/// Shared tunnel secret baked into the shipped binaries, matching the Worker's
/// global `TUNNEL_SECRET` — so the managed tunnel needs no operator credentials.
/// Mirrors greentic-start's `gtunnel::DEFAULT_TUNNEL_SECRET` (the crates don't
/// share a dependency); the two MUST stay in sync or setup and start would
/// register the same tunnel id under different secrets.
const DEFAULT_TUNNEL_SECRET: &str =
    "d00a7591949699785228c42504afa41ce168f84e4118bb127e47c5cb98e4dd90";

/// Everything the setup binary needs to bring up the Greentic self-hosted tunnel.
/// Derived by the caller (from tenant/team + env) so setup and start compute the
/// same tunnel id and share one agent.
#[derive(Clone, Debug)]
pub struct GtunnelSetupCtx {
    pub worker_url: String,
    /// When set (`GREENTIC_TUNNEL_BASE_DOMAIN`), use subdomain routing
    /// (`https://<tunnelId>.<base>`) so the WebChat SPA works at the host root.
    pub base_domain: Option<String>,
    /// Root-map mode (`GREENTIC_TUNNEL_ROOT_MAP`): the whole Worker host maps to
    /// this one tunnel at its root (cloudflared-style), so the WebChat SPA works
    /// on workers.dev with no custom domain. Ignored when `base_domain` is set.
    pub root_map: bool,
    pub tunnel_id: String,
    pub secret: String,
}

/// Derive a URL-path-safe tunnel id from the TENANT ALONE — `team` is
/// deliberately ignored.
///
/// The id becomes the first path segment of the public URL, and it must equal
/// the `<tenant>` segment of the WebChat URL space (`/v1/web/webchat/<tenant>/…`)
/// so the Worker routes the SPA's root-absolute calls to the right tunnel. That
/// URL space has no team segment, so folding `team` in here produced a
/// `<tenant>-<team>` id that could never match a webchat URL — and disagreed
/// with greentic-start, which keys on the tenant alone.
///
/// This is only the FALLBACK derivation now: once setup has run it persists the
/// resolved id to `.greentic/tunnel.json` and greentic-start reads it verbatim
/// rather than re-deriving (see [`crate::platform_setup::types::TunnelAnswers`]).
/// Kept in sync with greentic-start's `sanitize_tunnel_id` by convention — the
/// crates cannot share the rule via `greentic-types`, which is exact-pinned at
/// `=1.1.2` across this graph (the same reason `cli_args.rs` keeps a local copy
/// of `DEFAULT_TENANT`).
pub fn derive_gtunnel_id(tenant: &str, _team: &str) -> String {
    let base = sanitize_tunnel_id(tenant);
    match install_clash_suffix(&tunnel_state_root(), &base) {
        Some(suffix) => format!("{base}-{suffix}"),
        None => base,
    }
}

/// The managed-tunnel id for the application rooted at `bundle_path`, in
/// strict precedence order:
///
/// 1. the id already recorded in `.greentic/tunnel.json` — ALWAYS wins, so an
///    application keeps one id for its whole life. That id is baked into the
///    URLs registered with Slack, Webex and OAuth providers; re-deriving or
///    re-minting over it would strand every one of them.
/// 2. otherwise a freshly MINTED id (see [`mint_gtunnel_id`]), persisted
///    immediately so step 1 catches it from here on. This is the creation
///    point: two applications built from the same bundle under the same tenant
///    name must not land on the same public URL, which is exactly what the
///    deterministic derivation did.
/// 3. only if that write fails, [`derive_gtunnel_id`]. A minted id we cannot
///    persist would be different on every run, whereas the derivation is stable
///    for (install, tenant) — and it is byte-identical to what greentic-start
///    falls back to when no id is recorded, so the two binaries still agree.
///
/// Persisting BEFORE the tunnel is proven reachable is deliberate: the id must
/// survive a failed attempt, or the retry would mint a second one. Nothing is
/// registered against an unreachable tunnel anyway — setup bails first.
pub fn resolve_gtunnel_id_for_bundle(bundle_path: &Path, tenant: &str) -> String {
    if let Some(existing) = crate::platform_setup::load_tunnel_artifact(bundle_path)
        .ok()
        .flatten()
        .and_then(|answers| answers.tunnel_id)
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
    {
        return existing;
    }

    let minted = mint_gtunnel_id(tenant);
    let answers = crate::platform_setup::TunnelAnswers {
        // Mode is owned by the wizard/UI selection; persisting merges per field.
        mode: None,
        tunnel_id: Some(minted.clone()),
    };
    match crate::platform_setup::persist_tunnel_artifact(bundle_path, &answers) {
        Ok(path) => {
            eprintln!(
                "[setup tunnel-id] minted gtunnel id `{minted}` for this application in {} — \
                 it is now fixed for the life of the application",
                path.display()
            );
            minted
        }
        Err(err) => {
            let derived = derive_gtunnel_id(tenant, "");
            eprintln!(
                "warning: could not record a minted gtunnel id ({err:#}); falling back to the \
                 derived id `{derived}`, which greentic-start derives identically but which two \
                 applications on the same tenant would share"
            );
            derived
        }
    }
}

/// Mint a BRAND-NEW managed-tunnel id for `tenant`.
///
/// Shape is identical to [`derive_gtunnel_id`] — `<sanitized tenant>-<5 hex>` —
/// so minted and derived ids are interchangeable everywhere downstream: the
/// Worker's path routing, the `<tenant>` segment of the webchat URL space, and
/// greentic-start's served-tenant alias all assume that one shape.
///
/// Entropy is the OS-seeded CSPRNG behind `rand::random`, NOT the install seed
/// and NOT a clock. The seed-derived suffix is a pure function of
/// (install, tenant), so a second application created from the same bundle
/// under the same tenant name reproduced the first one's id and therefore its
/// public URL — the bug this exists to fix. A clock reading fails the same way
/// for two applications created within one tick, and degrades on a clock reset.
///
/// 5 hex is 20 bits, kept rather than widened because the width is part of the
/// id shape above. Two applications collide with probability ~2^-20; a
/// collision is loud rather than silent — the Worker permits one agent socket
/// per tunnel id, so the second application's tunnel is refused rather than
/// quietly stealing traffic.
pub fn mint_gtunnel_id(tenant: &str) -> String {
    format!(
        "{}-{:05x}",
        sanitize_tunnel_id(tenant),
        rand::random::<u32>() & 0x000f_ffff
    )
}

/// Slug of `tenant`: lowercase alphanumerics and `-`, everything else folded to
/// `-`, trimmed, empty falling back to `default`. Matches greentic-start's
/// `sanitize_tunnel_id`.
fn sanitize_tunnel_id(tenant: &str) -> String {
    let slug: String = tenant
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = slug.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "default".to_string()
    } else {
        trimmed
    }
}

/// 5-hex clash-avoidance suffix for `base`, derived from the per-install seed at
/// `<root>/instance-seed`.
///
/// The managed tunnel is ONE shared Worker, and almost every operator runs the
/// default tenant — so a bare `<tenant>` id means every install collides on the
/// same public path. The suffix is what keeps them distinct.
///
/// It must be STABLE, because it ends up inside URLs registered with Slack,
/// Webex and OAuth providers. It must also be the SAME value greentic-start
/// derives, which is why the seed lives in the tunnel state root both binaries
/// already share (alongside `secret` and `secrets/<tunnelId>`) and why the hash
/// is byte-compatible with start's `tenant_clash_suffix_from_seed`:
/// last 5 hex of `sha256(seed || 0x00 || base)`.
///
/// Returns `None` when no seed can be established, in which case the caller uses
/// the bare id — degraded (collidable) but functional, rather than failing setup.
fn install_clash_suffix(root: &Path, base: &str) -> Option<String> {
    let seed = load_or_create_instance_seed(root)?;
    let mut hasher = Sha256::new();
    hasher.update(seed.as_bytes());
    hasher.update([0u8]);
    hasher.update(base.as_bytes());
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Some(hex[hex.len() - 5..].to_string())
}

/// Read `<root>/instance-seed`, creating it on first use. Shared on-disk format
/// with greentic-start, which reads the same file so both derive one suffix.
///
/// Creation is exclusive (`create_new`), and a lost race re-reads the winner's
/// seed rather than overwriting it. A plain write let every concurrent caller
/// install its own random seed on a fresh install — last write won, so callers
/// that had already derived an id kept a suffix nobody else would reproduce.
/// The suffix ends up inside webhook URLs registered with Slack, Webex and
/// OAuth providers, and `gtc setup` and `gtc start` derive it independently, so
/// a divergent seed strands a registered URL.
fn load_or_create_instance_seed(root: &Path) -> Option<String> {
    let path = root.join("instance-seed");
    if let Some(seed) = read_instance_seed(&path) {
        return Some(seed);
    }
    let seed: String = (0..32)
        .map(|_| format!("{:02x}", rand::random::<u8>()))
        .collect();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    // Stage the whole seed, then publish it with a link. Creating the file
    // first and filling it after is not enough: a concurrent caller re-reading
    // the winner's file would find it still empty, judge it unusable and
    // overwrite it — the same last-write-wins outcome, in a narrower window.
    // Linking publishes an already-complete file, so a loser only ever reads a
    // seed it can safely adopt.
    let staged = path.with_file_name(format!("instance-seed.{:016x}.tmp", rand::random::<u64>()));
    std::fs::write(&staged, &seed).ok()?;
    let outcome = match std::fs::hard_link(&staged, &path) {
        Ok(()) => Some(seed),
        // Someone else published first: theirs wins, because they may already
        // have handed the derived id to a provider.
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            match read_instance_seed(&path) {
                Some(existing) => Some(existing),
                // Present but unusable (truncated, hand-edited): no id anyone
                // registered can have come from it, so replace it with ours.
                None => std::fs::rename(&staged, &path).ok().map(|()| seed),
            }
        }
        // No link support (some network/removable filesystems): fall back to a
        // rename, which is still atomic — it just cannot detect a loser.
        Err(_) => std::fs::rename(&staged, &path).ok().map(|()| seed),
    };
    let _ = std::fs::remove_file(&staged);
    outcome
}

/// The seed at `path`, or `None` when it is absent or not 64 hex chars.
fn read_instance_seed(path: &Path) -> Option<String> {
    let existing = std::fs::read_to_string(path).ok()?.trim().to_string();
    (existing.len() == 64 && existing.chars().all(|c| c.is_ascii_hexdigit())).then_some(existing)
}

impl GtunnelSetupCtx {
    /// Zero-config context: worker URL and tunnel secret both from env/default,
    /// so the managed tunnel works on a fresh box with no operator input. Secret
    /// resolution matches greentic-start's, so both binaries present the same
    /// credential for the same tunnel id.
    pub fn new(tunnel_id: String) -> Self {
        let secret = resolve_tunnel_secret(&tunnel_id);
        let base_domain = std::env::var("GREENTIC_TUNNEL_BASE_DOMAIN")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        // Root-map only applies without a base domain (a subdomain already gives
        // each tunnel its own host root), matching greentic-start's precedence.
        let root_map = base_domain.is_none() && env_flag("GREENTIC_TUNNEL_ROOT_MAP");
        Self {
            worker_url: gtunnel_worker_base_url(),
            base_domain,
            root_map,
            tunnel_id,
            secret,
        }
    }
}

/// Base URL of the managed-tunnel Worker: `GREENTIC_TUNNEL_WORKER_URL` when set
/// and non-empty, else [`DEFAULT_GTUNNEL_WORKER_BASE_URL`].
///
/// Shared deliberately by [`GtunnelSetupCtx::new`], which BUILDS the public URL,
/// and [`is_ephemeral_tunnel_url`], which decides whether an existing URL is one
/// of ours to re-point. Those two must resolve the same host: if the predicate
/// tested a different host than the builder used, a managed URL would once again
/// look permanent and never be refreshed — the exact bug this helper prevents.
fn gtunnel_worker_base_url() -> String {
    std::env::var("GREENTIC_TUNNEL_WORKER_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_GTUNNEL_WORKER_BASE_URL.to_string())
}

/// Truthy env flag: `1`, `true`, or `yes` (case-insensitive). Mirrors the same
/// helper in greentic-start so both binaries read `GREENTIC_TUNNEL_ROOT_MAP`
/// identically.
fn env_flag(key: &str) -> bool {
    std::env::var(key)
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes"
        })
        .unwrap_or(false)
}

/// `~/.greentic/tunnel` (override: `GREENTIC_TUNNEL_STATE_DIR`).
fn tunnel_state_root() -> PathBuf {
    std::env::var_os("GREENTIC_TUNNEL_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
            std::env::var_os(var)
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir)
                .join(".greentic")
                .join("tunnel")
        })
}

fn read_secret_file(path: PathBuf) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Resolve the tunnel secret: `GREENTIC_TUNNEL_SECRET` env > per-tunnel store
/// (`<root>/secrets/<id>`) > operator secret (`<root>/secret`) >
/// [`DEFAULT_TUNNEL_SECRET`]. Never empty — the baked-in constant is the whole
/// point: the managed tunnel authenticates with no operator credentials at all.
/// Must stay identical to greentic-start's `gtunnel::resolve_secret`, since
/// either binary can be the one that spawns the agent for a given tunnel id.
fn resolve_tunnel_secret(tunnel_id: &str) -> String {
    if let Ok(secret) = std::env::var("GREENTIC_TUNNEL_SECRET")
        && !secret.is_empty()
    {
        return secret;
    }
    resolve_tunnel_secret_in(&tunnel_state_root(), tunnel_id)
}

/// File-store half of [`resolve_tunnel_secret`], root passed explicitly so it is
/// testable without touching the developer's real `~/.greentic/tunnel`.
fn resolve_tunnel_secret_in(root: &Path, tunnel_id: &str) -> String {
    read_secret_file(root.join("secrets").join(tunnel_id))
        .or_else(|| read_secret_file(root.join("secret")))
        .unwrap_or_else(|| DEFAULT_TUNNEL_SECRET.to_string())
}

fn gtunnel_ctx_from_env() -> GtunnelSetupCtx {
    GtunnelSetupCtx::new(
        std::env::var("GREENTIC_TUNNEL_ID").unwrap_or_else(|_| "default".to_string()),
    )
}

pub fn should_start_setup_tunnel(mode: &str, answers: &JsonMap<String, Value>) -> bool {
    matches!(mode, "cloudflared" | "ngrok" | "gtunnel")
        && answers.values().any(|provider_answers| {
            let Some(obj) = provider_answers.as_object() else {
                return false;
            };
            crate::provider_state::provider_enabled_from_map(obj)
                && !obj
                    .get("public_base_url")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .is_some_and(|value| {
                        value.starts_with("https://") && !is_ephemeral_tunnel_url(value)
                    })
        })
}

pub fn start_setup_tunnel(
    mode: &str,
    local_base_url: &str,
    gtunnel: Option<GtunnelSetupCtx>,
) -> Result<SetupTunnel> {
    match mode {
        "cloudflared" => start_cloudflared_shared(local_base_url),
        "ngrok" => {
            let (child, url) = spawn_tunnel_process(mode, local_base_url)?;
            Ok(SetupTunnel {
                mode: mode.to_string(),
                local_base_url: local_base_url.trim_end_matches('/').to_string(),
                public_base_url: url,
                child: Some(child),
                kill_on_drop: true,
            })
        }
        "gtunnel" => {
            let ctx = gtunnel.unwrap_or_else(gtunnel_ctx_from_env);
            start_gtunnel_shared(local_base_url, &ctx)
        }
        other => Err(anyhow!("unsupported setup tunnel mode: {other}")),
    }
}

/// Adopt (or spawn) the Greentic self-hosted tunnel agent under the machine-wide
/// shared record for this port, so `greentic-start` reuses the same agent. The
/// public URL is deterministic (`<worker>/<tunnel_id>`) — no discovery needed.
fn start_gtunnel_shared(local_base_url: &str, ctx: &GtunnelSetupCtx) -> Result<SetupTunnel> {
    let mode = "gtunnel";
    let port = crate::shared_tunnel::local_port_from_base_url(local_base_url)
        .ok_or_else(|| anyhow!("cannot derive a local port from {local_base_url}"))?;
    let paths = crate::shared_tunnel::shared_service_tunnel_paths(mode, port);
    let _lock =
        crate::shared_tunnel::TunnelLock::acquire(&paths.lock_path, Duration::from_secs(30))?;

    let public_base_url = match &ctx.base_domain {
        Some(base) => format!("https://{}.{}", ctx.tunnel_id, base.trim_matches('.')),
        // Root-map: the Worker host itself is this tunnel — no path prefix.
        None if ctx.root_map => ctx.worker_url.trim_end_matches('/').to_string(),
        None => format!("{}/{}", ctx.worker_url.trim_end_matches('/'), ctx.tunnel_id),
    };

    // Reuse a live agent (ours or greentic-start's) rather than spawn a second —
    // the Worker allows only one socket per tunnel id. But a pid can be alive
    // while its WebSocket to the Worker is dead, so only adopt an agent that is
    // actually SERVING; a stale one is killed and replaced (so the caller's
    // reachability probe doesn't just fail on a reused-but-dead tunnel).
    let (recorded_pid, _recorded_url) = crate::shared_tunnel::read_record(&paths);
    if let Some(pid) = recorded_pid
        && crate::shared_tunnel::process_alive(pid)
    {
        if gtunnel_serving(&public_base_url) {
            eprintln!("Reusing shared {mode} agent (pid {pid}): {public_base_url}");
            let _ = crate::shared_tunnel::write_record(&paths, pid, &public_base_url);
            return Ok(reuse_shared_tunnel(mode, local_base_url, public_base_url));
        }
        eprintln!(
            "Shared {mode} agent (pid {pid}) is alive but {public_base_url} is not serving — \
             replacing the stale tunnel"
        );
        crate::shared_tunnel::terminate_recorded_pid_named(pid, "greentic-start");
    }
    crate::shared_tunnel::clear_record(&paths);

    let child = spawn_gtunnel_agent(local_base_url, ctx, &paths.log_path)?;
    if let Err(err) = crate::shared_tunnel::write_record(&paths, child.id(), &public_base_url) {
        eprintln!("warning: could not publish shared gtunnel record: {err:#}");
    }
    eprintln!("Setup tunnel started via {mode}: {public_base_url}");
    Ok(SetupTunnel {
        mode: mode.to_string(),
        local_base_url: local_base_url.trim_end_matches('/').to_string(),
        public_base_url,
        child: Some(child),
        kill_on_drop: false,
    })
}

/// Whether the tunnel serves end to end: a bounded GET to the public URL. A
/// routed response (2xx/3xx/4xx) proves the agent is connected and forwarding;
/// the Worker's `502 tunnel offline` (or any 5xx / transport error) means the
/// recorded agent is stale. `< 500` mirrors `setup_backend_public_tunnel_responds`.
fn gtunnel_serving(public_url: &str) -> bool {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .build()
        .new_agent();
    match agent.get(public_url).call() {
        Ok(_) => true,
        Err(ureq::Error::StatusCode(code)) => code < 500,
        Err(_) => false,
    }
}

/// Spawn `greentic-start __tunnel-agent`, forwarding to `local_base_url`, with
/// stdout/stderr redirected to the shared tunnel log.
fn spawn_gtunnel_agent(
    local_base_url: &str,
    ctx: &GtunnelSetupCtx,
    log_path: &Path,
) -> Result<Child> {
    let binary = resolve_gtunnel_agent_binary()?;
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("open gtunnel log {}", log_path.display()))?;
    let log_err = log
        .try_clone()
        .with_context(|| "clone gtunnel log handle")?;

    let edge_url = match &ctx.base_domain {
        // Subdomain routing: register at the tunnel's own host root.
        Some(base) => format!("wss://{}.{}/_tunnel", ctx.tunnel_id, base.trim_matches('.')),
        None => {
            let ws_base = if let Some(rest) = ctx
                .worker_url
                .trim_end_matches('/')
                .strip_prefix("https://")
            {
                format!("wss://{rest}")
            } else if let Some(rest) = ctx.worker_url.trim_end_matches('/').strip_prefix("http://")
            {
                format!("ws://{rest}")
            } else {
                ctx.worker_url.trim_end_matches('/').to_string()
            };
            if ctx.root_map {
                // Root-map: register at the Worker host root; the Worker maps
                // every request (including /_tunnel) to TUNNEL_DEFAULT_ID.
                format!("{ws_base}/_tunnel")
            } else {
                format!("{ws_base}/{}/_tunnel", ctx.tunnel_id)
            }
        }
    };

    Command::new(&binary)
        .arg("__tunnel-agent")
        .env("GREENTIC_TUNNEL_EDGE_URL", edge_url)
        .env("GREENTIC_TUNNEL_SECRET", &ctx.secret)
        .env(
            "GREENTIC_TUNNEL_TARGET",
            local_base_url.trim_end_matches('/'),
        )
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .spawn()
        .with_context(|| format!("spawn gtunnel agent via {}", binary.display()))
}

/// Locate the `greentic-start` binary that hosts the `__tunnel-agent` subcommand:
/// explicit `GREENTIC_TUNNEL_AGENT_BIN`, else the first `greentic-start` on PATH.
fn resolve_gtunnel_agent_binary() -> Result<PathBuf> {
    if let Some(explicit) = std::env::var_os("GREENTIC_TUNNEL_AGENT_BIN") {
        let path = PathBuf::from(explicit);
        if path.exists() {
            return Ok(path);
        }
        return Err(anyhow!(
            "GREENTIC_TUNNEL_AGENT_BIN points at {}, which does not exist",
            path.display()
        ));
    }
    resolve_path_binary("greentic-start").ok_or_else(|| {
        anyhow!(
            "greentic-start not found on PATH (needed to run the tunnel agent); \
             set GREENTIC_TUNNEL_AGENT_BIN to its location"
        )
    })
}

/// Build a [`SetupTunnel`] that reuses an already-running shared tunnel: there
/// is no child to own and nothing to kill on drop — the tunnel deliberately
/// outlives this setup process so the runtime it configures keeps the same URL.
fn reuse_shared_tunnel(mode: &str, local_base_url: &str, public_base_url: String) -> SetupTunnel {
    SetupTunnel::detached(mode, local_base_url, &public_base_url)
}

/// Acquire the machine-wide shared cloudflared tunnel for the port behind
/// `local_base_url`: reuse the recorded one when it still serves, otherwise
/// spawn a fresh cloudflared and publish it so greentic-start adopts the same
/// tunnel instead of racing it (see [`crate::shared_tunnel`]).
fn start_cloudflared_shared(local_base_url: &str) -> Result<SetupTunnel> {
    let mode = "cloudflared";
    let port = crate::shared_tunnel::local_port_from_base_url(local_base_url)
        .ok_or_else(|| anyhow!("cannot derive a local port from {local_base_url}"))?;
    let paths = crate::shared_tunnel::shared_tunnel_paths(port);
    let _lock =
        crate::shared_tunnel::TunnelLock::acquire(&paths.lock_path, Duration::from_secs(45))?;

    use crate::shared_tunnel::RecordedTunnelState;
    let (recorded_pid, recorded_url) = crate::shared_tunnel::read_record(&paths);
    eprintln!(
        "Setup tunnel: checking shared cloudflared record for port {port} \
         (recorded pid={recorded_pid:?}, url={recorded_url:?})"
    );
    if let Some(url) = recorded_url {
        match crate::shared_tunnel::classify_recorded_tunnel(&paths, recorded_pid, &url) {
            RecordedTunnelState::Serving | RecordedTunnelState::WarmingUp => {
                eprintln!("Reusing shared {mode} tunnel: {url}");
                return Ok(reuse_shared_tunnel(mode, local_base_url, url));
            }
            RecordedTunnelState::Down => {
                // Recorded tunnel is genuinely gone (process dead, or the edge
                // returned 530 for a lost binding). It is ours to replace: the
                // pid came from the shared record, never a process-name match.
                eprintln!("Shared {mode} tunnel {url} is down; replacing it");
                if let Some(pid) = recorded_pid {
                    crate::shared_tunnel::terminate_recorded_pid(pid);
                }
            }
        }
    }
    crate::shared_tunnel::clear_record(&paths);

    let (child, url) = spawn_cloudflared_logged(local_base_url, &paths.log_path)?;
    if let Err(err) = crate::shared_tunnel::write_record(&paths, child.id(), &url) {
        eprintln!("warning: could not publish shared tunnel record: {err:#}");
    }
    eprintln!("Setup tunnel started via {mode}: {url}");
    Ok(SetupTunnel {
        mode: mode.to_string(),
        local_base_url: local_base_url.trim_end_matches('/').to_string(),
        public_base_url: url,
        child: Some(child),
        kill_on_drop: false,
    })
}

/// Spawn cloudflared with stdout/stderr redirected to the shared log file and
/// discover the tunnel URL by polling that file.
///
/// Deliberately NOT piped: cloudflared is a Go binary, and Go processes die
/// on SIGPIPE when writing logs to a closed stdout/stderr pipe — a piped
/// tunnel would be killed the moment setup exits, defeating the
/// outlive-setup handoff. A log file keeps it alive and doubles as
/// greentic-start's fallback URL-discovery source.
fn spawn_cloudflared_logged(local_base_url: &str, log_path: &Path) -> Result<(Child, String)> {
    let binary = resolve_tunnel_binary("cloudflared")?;
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create tunnel log dir {}", parent.display()))?;
    }
    // Truncate: URL discovery must not read a previous tunnel's URL.
    let log = std::fs::File::create(log_path)
        .with_context(|| format!("create tunnel log {}", log_path.display()))?;
    let log_err = log
        .try_clone()
        .with_context(|| format!("clone tunnel log handle {}", log_path.display()))?;

    let mut child = Command::new(binary)
        .args(["tunnel", "--url", local_base_url, "--no-autoupdate"])
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .spawn()
        .with_context(|| "start cloudflared setup tunnel")?;

    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    while std::time::Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            return Err(anyhow!(
                "cloudflared exited before publishing a URL: {status} (log: {})",
                log_path.display()
            ));
        }
        if let Ok(contents) = std::fs::read_to_string(log_path)
            && let Some(url) = extract_tunnel_https_url("cloudflared", &contents)
        {
            return Ok((child, url));
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    let _ = child.kill();
    let _ = child.wait();
    Err(anyhow!(
        "cloudflared did not publish an https:// URL within 25 seconds (log: {})",
        log_path.display()
    ))
}

/// Spawn the tunnel binary and read its stdout/stderr until it publishes an
/// https:// URL for its mode.
fn spawn_tunnel_process(mode: &str, local_base_url: &str) -> Result<(Child, String)> {
    let mut command = match mode {
        "cloudflared" => {
            let binary = resolve_tunnel_binary(mode)?;
            let mut command = Command::new(binary);
            command.args(["tunnel", "--url", local_base_url, "--no-autoupdate"]);
            command
        }
        "ngrok" => {
            let binary = resolve_tunnel_binary(mode)?;
            let mut command = Command::new(binary);
            command.args(["http", local_base_url, "--log=stdout"]);
            command
        }
        other => return Err(anyhow!("unsupported setup tunnel mode: {other}")),
    };
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .with_context(|| format!("start {mode} setup tunnel"))?;

    let (tx, rx) = std::sync::mpsc::channel::<String>();
    if let Some(stdout) = child.stdout.take() {
        spawn_tunnel_log_reader(stdout, tx.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_tunnel_log_reader(stderr, tx.clone());
    }
    drop(tx);

    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    while std::time::Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            return Err(anyhow!("{mode} exited before publishing a URL: {status}"));
        }
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(line) => {
                if let Some(url) = extract_tunnel_https_url(mode, &line) {
                    eprintln!("Setup tunnel started via {mode}: {url}");
                    return Ok((child, url));
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    Err(anyhow!(
        "{mode} did not publish an https:// URL within 25 seconds"
    ))
}

fn resolve_tunnel_binary(mode: &str) -> Result<PathBuf> {
    match mode {
        "cloudflared" => resolve_cloudflared_binary(),
        "ngrok" => resolve_path_binary("ngrok")
            .ok_or_else(|| anyhow!("ngrok is not installed or not on PATH")),
        other => Err(anyhow!("unsupported setup tunnel mode: {other}")),
    }
}

fn resolve_cloudflared_binary() -> Result<PathBuf> {
    if let Some(binary) = resolve_path_binary("cloudflared") {
        return Ok(binary);
    }

    let binary = managed_tunnel_binary_path("cloudflared");
    if executable_exists(&binary) {
        return Ok(binary);
    }

    install_cloudflared_binary(&binary)?;
    Ok(binary)
}

fn resolve_path_binary(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(platform_executable_name(name)))
        .find(|candidate| executable_exists(candidate))
}

fn managed_tunnel_binary_path(name: &str) -> PathBuf {
    let base_dir = std::env::var_os("GREENTIC_SETUP_BIN_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".cache").join("greentic-setup").join("bin"))
        })
        .unwrap_or_else(|| std::env::temp_dir().join("greentic-setup").join("bin"));
    base_dir.join(platform_executable_name(name))
}

fn platform_executable_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn executable_exists(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn install_cloudflared_binary(target: &Path) -> Result<()> {
    let asset = cloudflared_release_asset()
        .ok_or_else(|| anyhow!("cloudflared auto-install is unsupported on this platform"))?;
    let download_url =
        format!("https://github.com/cloudflare/cloudflared/releases/latest/download/{asset}");

    let parent = target
        .parent()
        .ok_or_else(|| anyhow!("invalid managed cloudflared path {}", target.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create tunnel binary cache {}", parent.display()))?;
    let temp_path = target.with_extension(format!("download-{}", std::process::id()));
    let bytes = download_bytes(&download_url)
        .with_context(|| format!("download cloudflared release asset {asset}"))?;
    if asset.ends_with(".tgz") {
        extract_cloudflared_tgz(&bytes, target)?;
    } else {
        std::fs::write(&temp_path, bytes)
            .with_context(|| format!("write {}", temp_path.display()))?;
        finalize_installed_binary(&temp_path, target)?;
    }

    Ok(())
}

fn download_bytes(url: &str) -> Result<Vec<u8>> {
    let mut response = crate::http_client::download_agent()
        .get(url)
        .call()
        .map_err(|err| anyhow!("request {url}: {err}"))?;
    response
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_vec()
        .map_err(|err| anyhow!("read {url}: {err}"))
}

fn extract_cloudflared_tgz(bytes: &[u8], target: &Path) -> Result<()> {
    let temp_path = target.with_extension(format!("download-{}", std::process::id()));
    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries().context("read cloudflared archive")? {
        let mut entry = entry.context("read cloudflared archive entry")?;
        let path = entry.path().context("read cloudflared archive path")?;
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name == "cloudflared" || name == "cloudflared.exe" {
            let mut output = std::fs::File::create(&temp_path)
                .with_context(|| format!("create {}", temp_path.display()))?;
            std::io::copy(&mut entry, &mut output)
                .with_context(|| format!("extract {}", temp_path.display()))?;
            finalize_installed_binary(&temp_path, target)?;
            return Ok(());
        }
    }
    Err(anyhow!("cloudflared archive did not contain a binary"))
}

fn finalize_installed_binary(temp_path: &Path, target: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(temp_path)
            .with_context(|| format!("stat {}", temp_path.display()))?
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(temp_path, permissions)
            .with_context(|| format!("chmod {}", temp_path.display()))?;
    }
    std::fs::rename(temp_path, target)
        .with_context(|| format!("install cloudflared to {}", target.display()))?;
    Ok(())
}

fn cloudflared_release_asset() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("cloudflared-darwin-arm64.tgz"),
        ("macos", "x86_64") => Some("cloudflared-darwin-amd64.tgz"),
        ("linux", "aarch64") => Some("cloudflared-linux-arm64"),
        ("linux", "x86_64") => Some("cloudflared-linux-amd64"),
        ("windows", "x86_64") => Some("cloudflared-windows-amd64.exe"),
        ("windows", "x86") => Some("cloudflared-windows-386.exe"),
        _ => None,
    }
}

fn spawn_tunnel_log_reader<R>(stream: R, tx: std::sync::mpsc::Sender<String>)
where
    R: std::io::Read + Send + 'static,
{
    std::thread::spawn(move || {
        use std::io::BufRead;
        let reader = std::io::BufReader::new(stream);
        for line in reader.lines().map_while(std::result::Result::ok) {
            let _ = tx.send(line);
        }
    });
}

pub fn extract_tunnel_https_url(mode: &str, line: &str) -> Option<String> {
    extract_https_urls(line)
        .into_iter()
        .find(|url| tunnel_url_matches_mode(mode, url))
}

fn tunnel_url_matches_mode(mode: &str, url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    if parsed.scheme() != "https" {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    match mode {
        "cloudflared" => host == "trycloudflare.com" || host.ends_with(".trycloudflare.com"),
        "ngrok" => host.ends_with(".ngrok-free.app") || host.ends_with(".ngrok.io"),
        _ => false,
    }
}

fn extract_https_urls(line: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut offset = 0;
    while let Some(start) = line[offset..].find("https://") {
        let absolute_start = offset + start;
        let tail = &line[absolute_start..];
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | ',' | ')'))
            .unwrap_or(tail.len());
        urls.push(tail[..end].trim_end_matches('/').to_string());
        offset = absolute_start + end;
    }
    urls
}

pub fn inject_setup_public_base_url(answers: &mut JsonMap<String, Value>, public_base_url: &str) {
    // The OAuth *callback* (developer app-install) is served by the setup server,
    // not the runtime, so provider ops that register OAuth redirect URLs need the
    // setup server's public URL — separate from the messaging `public_base_url`.
    // Injected only when `GREENTIC_SETUP_PUBLIC_BASE_URL` is set; otherwise ops
    // fall back to `public_base_url` for back-compat.
    let oauth_callback_base_url = std::env::var("GREENTIC_SETUP_PUBLIC_BASE_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| value.starts_with("https://"));
    for provider_answers in answers.values_mut() {
        let Some(obj) = provider_answers.as_object_mut() else {
            continue;
        };
        if !crate::provider_state::provider_enabled_from_map(obj) {
            continue;
        }
        if let Some(ref callback_base) = oauth_callback_base_url {
            obj.insert(
                "oauth_callback_base_url".to_string(),
                Value::String(callback_base.clone()),
            );
        }
        if obj
            .get("public_base_url")
            .and_then(Value::as_str)
            .map(str::trim)
            .is_some_and(|value| value.starts_with("https://") && !is_ephemeral_tunnel_url(value))
        {
            continue;
        }
        obj.insert(
            "public_base_url".to_string(),
            Value::String(public_base_url.to_string()),
        );
    }
}

/// Does `value` name a tunnel URL that SETUP HANDED OUT and may therefore
/// re-point? Callers use it to tell an operator's permanent URL (preserve — never
/// touch it) from one we produced ourselves (replace with the current one).
///
/// NOTE ON THE NAME: "ephemeral" here means "engine-assigned, ours to replace" —
/// NOT "short-lived". The managed-tunnel Worker HOST is perfectly stable, yet the
/// tunnel id in its first path segment changes between runs, so a managed URL is
/// still refreshable and must be matched here. The name is kept because this is
/// `pub` and consumed by three other modules, and because two payload keys embed
/// it — `public_base_url_is_ephemeral_tunnel` (persisted in `runtime_context` and
/// compared against earlier runs) and `"ephemeral"` in the setup-machine step
/// detail. Renaming the function without those keys would mismatch, and renaming
/// the keys would invalidate already-persisted state for no behavioural gain.
///
/// Before this matched the Worker host, a STALE managed URL looked permanent and
/// was preserved forever: one real session left Webex and the state provider on
/// `<worker>/default` while Slack moved to `<worker>/default-default`, and the
/// webhook Webex had registered pointed at a path the tunnel no longer served.
pub fn is_ephemeral_tunnel_url(value: &str) -> bool {
    is_ephemeral_tunnel_url_for_worker_base(value, &gtunnel_worker_base_url())
}

/// Worker-base half of [`is_ephemeral_tunnel_url`], with the base passed
/// explicitly so it is testable without mutating process env — the same pattern
/// [`resolve_tunnel_secret_in`] uses for the state root.
fn is_ephemeral_tunnel_url_for_worker_base(value: &str, worker_base_url: &str) -> bool {
    // Derived from the configured Worker base rather than a hardcoded hostname,
    // so a self-hosted Worker is treated exactly like the default one.
    let managed_host = url::Url::parse(worker_base_url)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.to_ascii_lowercase()));
    url::Url::parse(value).ok().is_some_and(|url| {
        url.scheme() == "https"
            && url.host_str().is_some_and(|host| {
                let host = host.to_ascii_lowercase();
                host == "trycloudflare.com"
                    || host.ends_with(".trycloudflare.com")
                    || host.ends_with(".ngrok-free.app")
                    || host.ends_with(".ngrok.io")
                    // Any URL on the managed Worker host is ours: the host does
                    // not distinguish tunnel ids, the path segment does, and that
                    // segment is exactly what goes stale.
                    || managed_host.as_deref() == Some(host.as_str())
            })
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::{Map as JsonMap, Value, json};

    use super::*;

    #[test]
    fn default_tunnel_secret_is_64_hex_chars() {
        assert_eq!(DEFAULT_TUNNEL_SECRET.len(), 64, "256-bit secret as hex");
        assert!(DEFAULT_TUNNEL_SECRET.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn resolve_tunnel_secret_falls_back_to_baked_in_constant() {
        // Empty store → the shipped constant, never empty: setup must never
        // demand credentials for the managed tunnel.
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            resolve_tunnel_secret_in(dir.path(), "demo-default"),
            DEFAULT_TUNNEL_SECRET
        );
    }

    #[test]
    fn resolve_tunnel_secret_prefers_per_tunnel_file_then_operator_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("secret"), "operator-secret\n").expect("write operator");
        assert_eq!(
            resolve_tunnel_secret_in(dir.path(), "demo-default"),
            "operator-secret"
        );

        std::fs::create_dir_all(dir.path().join("secrets")).expect("mkdir");
        std::fs::write(
            dir.path().join("secrets").join("demo-default"),
            "per-tunnel",
        )
        .expect("write per-tunnel");
        assert_eq!(
            resolve_tunnel_secret_in(dir.path(), "demo-default"),
            "per-tunnel"
        );
    }

    #[test]
    fn sanitize_tunnel_id_uses_the_tenant_alone() {
        // The BASE is the tenant alone — team is ignored, so it can equal the
        // <tenant> segment of /v1/web/webchat/<tenant>/… (which has no team
        // segment) and match greentic-start's sanitize_tunnel_id.
        assert_eq!(sanitize_tunnel_id("Acme Corp"), "acme-corp");
        assert_eq!(sanitize_tunnel_id("demo"), "demo");
        assert_eq!(sanitize_tunnel_id(""), "default");
        // Each non-alphanumeric folds to its own `-`, so runs are preserved
        // and only the edges are trimmed.
        assert_eq!(sanitize_tunnel_id("--Weird__Tenant--"), "weird--tenant");
    }

    #[test]
    fn derive_gtunnel_id_appends_a_clash_suffix_to_the_base() {
        // The full id carries a per-install suffix: the managed tunnel is ONE
        // shared Worker and nearly every operator runs the default tenant, so a
        // bare `<tenant>` id would collide across installs.
        let id = derive_gtunnel_id("demo", "default");
        let (base, suffix) = id.rsplit_once('-').expect("id carries a suffix");
        assert_eq!(base, "demo");
        assert_eq!(suffix.len(), 5, "5-hex suffix, got {id}");
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()), "{id}");
    }

    #[test]
    fn concurrent_first_use_settles_on_one_instance_seed() {
        // The failure this guards: on a fresh install several callers reach an
        // absent `instance-seed` at once. With a plain write each installed its
        // own random seed and the last write won, so callers that had already
        // derived a suffix kept one nobody could reproduce — and that suffix
        // sits inside webhook URLs registered with Slack, Webex and OAuth
        // providers. Exclusive creation makes the first writer win for everyone.
        //
        // Takes the root as an argument rather than mutating
        // `GREENTIC_TUNNEL_STATE_DIR`, which is process-global and would race
        // every other test the way the seed itself used to.
        let root = tempfile::tempdir().expect("seed root");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let suffixes: std::collections::BTreeSet<String> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let barrier = std::sync::Arc::clone(&barrier);
                    let path = root.path().to_path_buf();
                    scope.spawn(move || {
                        barrier.wait();
                        install_clash_suffix(&path, "acme").expect("suffix")
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("thread"))
                .collect()
        });
        assert_eq!(
            suffixes.len(),
            1,
            "concurrent first use must settle on one seed: {suffixes:?}"
        );
        // And the settled seed is the one on disk, so later processes agree.
        assert_eq!(
            install_clash_suffix(root.path(), "acme").as_ref(),
            suffixes.iter().next(),
            "the persisted seed must reproduce the suffix callers already used"
        );
    }

    #[test]
    fn instance_seed_is_healed_when_the_file_is_unusable() {
        // A truncated or hand-edited seed cannot have produced any registered
        // id, so it is replaced rather than propagated as `None` (which would
        // silently drop the suffix from the tunnel id).
        let root = tempfile::tempdir().expect("seed root");
        std::fs::write(root.path().join("instance-seed"), "not-a-seed").expect("write");
        let suffix = install_clash_suffix(root.path(), "acme").expect("suffix");
        assert_eq!(suffix.len(), 5, "{suffix}");
        assert_eq!(
            install_clash_suffix(root.path(), "acme").as_deref(),
            Some(suffix.as_str()),
            "the healed seed must be stable across calls"
        );
    }

    #[test]
    fn derive_gtunnel_id_ignores_team_entirely() {
        // Guards the regression directly: any team must collapse to one id, so a
        // second team on the same tenant cannot strand a registered webhook URL.
        let ids: std::collections::BTreeSet<String> = ["default", "eng", "", "Team B"]
            .iter()
            .map(|team| derive_gtunnel_id("acme", team))
            .collect();
        assert_eq!(
            ids.len(),
            1,
            "team must not influence the tunnel id: {ids:?}"
        );
        assert!(
            ids.iter().next().expect("one id").starts_with("acme-"),
            "{ids:?}"
        );
    }

    // ---- minting and per-application id resolution ----

    #[test]
    fn minted_ids_have_the_derived_shape_but_are_unique_per_call() {
        // Same shape as `derive_gtunnel_id`, so minted and derived ids are
        // interchangeable in the URL space and in greentic-start's alias.
        let ids: std::collections::BTreeSet<String> =
            (0..64).map(|_| mint_gtunnel_id("Acme Corp")).collect();
        for id in &ids {
            let (base, suffix) = id.rsplit_once('-').expect("id carries a suffix");
            assert_eq!(base, "acme-corp");
            assert_eq!(suffix.len(), 5, "5-hex suffix, got {id}");
            assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()), "{id}");
        }
        // The whole point: creating a second application under the same tenant
        // name must not reproduce the first one's URL. 64 draws from 2^20 will
        // collide with probability ~1/500, so a handful of duplicates is fine
        // but a single value is not.
        assert!(ids.len() > 32, "minting must not be deterministic: {ids:?}");
    }

    #[test]
    fn resolving_mints_once_and_then_adopts_the_recorded_id() {
        let bundle = tempfile::tempdir().expect("tempdir");
        let first = resolve_gtunnel_id_for_bundle(bundle.path(), "demo");
        assert!(first.starts_with("demo-"), "{first}");
        assert_eq!(
            crate::platform_setup::load_tunnel_artifact(bundle.path())
                .expect("load")
                .expect("artifact")
                .tunnel_id
                .as_deref(),
            Some(first.as_str()),
            "the minted id must be persisted at creation"
        );

        // Stable for the life of the application, however often setup re-runs.
        for _ in 0..5 {
            assert_eq!(resolve_gtunnel_id_for_bundle(bundle.path(), "demo"), first);
        }
        // Even if the tenant is later renamed: the registered URLs still carry
        // the old id, so the recorded value wins over any re-derivation.
        assert_eq!(resolve_gtunnel_id_for_bundle(bundle.path(), "other"), first);
    }

    #[test]
    fn two_applications_from_one_tenant_get_different_ids() {
        // The regression: `derive_gtunnel_id` is a pure function of
        // (install, tenant), so a second application built from the same bundle
        // under the same tenant name served the first one's public URL.
        let a = tempfile::tempdir().expect("tempdir");
        let b = tempfile::tempdir().expect("tempdir");
        assert_ne!(
            resolve_gtunnel_id_for_bundle(a.path(), "default"),
            resolve_gtunnel_id_for_bundle(b.path(), "default"),
        );
    }

    #[test]
    fn resolving_preserves_the_recorded_tunnel_mode() {
        let bundle = tempfile::tempdir().expect("tempdir");
        crate::platform_setup::persist_tunnel_artifact(
            bundle.path(),
            &crate::platform_setup::TunnelAnswers {
                mode: Some("gtunnel".to_string()),
                ..Default::default()
            },
        )
        .expect("seed mode");

        let id = resolve_gtunnel_id_for_bundle(bundle.path(), "demo");

        let saved = crate::platform_setup::load_tunnel_artifact(bundle.path())
            .expect("load")
            .expect("artifact");
        assert_eq!(saved.tunnel_id.as_deref(), Some(id.as_str()));
        assert_eq!(saved.mode.as_deref(), Some("gtunnel"));
    }

    // ---- per-install clash suffix ----

    #[test]
    fn clash_suffix_is_stable_across_calls_and_distinct_per_tenant() {
        // Stability is the whole point: the suffix ends up inside URLs registered
        // with Slack/Webex/OAuth, so it must survive restarts. It previously came
        // from a per-process nonce and changed on every start.
        let dir = tempfile::tempdir().expect("tempdir");
        let first = install_clash_suffix(dir.path(), "demo").expect("suffix");
        let second = install_clash_suffix(dir.path(), "demo").expect("suffix");
        assert_eq!(first, second, "same install + tenant must give one suffix");
        assert_eq!(first.len(), 5);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));

        // Distinct per tenant, so two tenants on one install do not collide.
        let other = install_clash_suffix(dir.path(), "acme").expect("suffix");
        assert_ne!(first, other, "suffix must be tenant-scoped");
    }

    /// Known-answer vectors for the FALLBACK derivation, pinning the exact
    /// bytes fed to sha256. greentic-start carries the identical test against
    /// the identical constants (`gtunnel::tests::
    /// clash_suffix_known_answers_match_greentic_setup`). The two crates cannot
    /// share the rule through `greentic-types` (exact-pinned at `=1.1.2` across
    /// this graph), so this pair of tests is what keeps them byte-identical:
    /// change one side's hashing and the other side's assertion fails.
    #[test]
    fn clash_suffix_known_answers_match_greentic_start() {
        const SEED: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("instance-seed"), SEED).expect("write seed");
        assert_eq!(
            install_clash_suffix(dir.path(), "acme").as_deref(),
            Some("0c7fe")
        );
        assert_eq!(
            install_clash_suffix(dir.path(), "default").as_deref(),
            Some("871fd")
        );
    }

    #[test]
    fn clash_suffix_differs_across_installs() {
        // Two installs must not land on the same public URL — that is the
        // collision this exists to prevent.
        let a = tempfile::tempdir().expect("tempdir");
        let b = tempfile::tempdir().expect("tempdir");
        assert_ne!(
            install_clash_suffix(a.path(), "default").expect("suffix"),
            install_clash_suffix(b.path(), "default").expect("suffix"),
            "distinct seeds must yield distinct suffixes"
        );
    }

    #[test]
    fn instance_seed_is_persisted_and_reused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = load_or_create_instance_seed(dir.path()).expect("seed");
        assert_eq!(first.len(), 64, "256-bit seed as hex");
        assert!(dir.path().join("instance-seed").is_file(), "must persist");
        assert_eq!(
            load_or_create_instance_seed(dir.path()).as_deref(),
            Some(first.as_str()),
            "a second read must reuse the persisted seed, not mint a new one"
        );
    }

    #[test]
    fn corrupt_seed_file_is_replaced_rather_than_used() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("instance-seed"), "not-a-seed\n").expect("write");
        let seed = load_or_create_instance_seed(dir.path()).expect("seed");
        assert_eq!(seed.len(), 64);
        assert!(seed.chars().all(|c| c.is_ascii_hexdigit()));
    }

    // ---- should_start_setup_tunnel ----

    #[test]
    fn setup_tunnel_helpers_detect_public_url_need() {
        let empty_answers = serde_json::from_value::<JsonMap<String, Value>>(json!({
            "messaging-slack": {}
        }))
        .expect("answers");
        assert!(should_start_setup_tunnel("cloudflared", &empty_answers));

        let https_answers = serde_json::from_value::<JsonMap<String, Value>>(json!({
            "messaging-slack": {
                "public_base_url": "https://operator.example.com"
            }
        }))
        .expect("answers");
        assert!(!should_start_setup_tunnel("cloudflared", &https_answers));
        let stale_tunnel_answers = serde_json::from_value::<JsonMap<String, Value>>(json!({
            "messaging-slack": {
                "public_base_url": "https://old.trycloudflare.com"
            }
        }))
        .expect("answers");
        assert!(should_start_setup_tunnel(
            "cloudflared",
            &stale_tunnel_answers
        ));
        assert!(!should_start_setup_tunnel("off", &empty_answers));
        let disabled_answers = serde_json::from_value::<JsonMap<String, Value>>(json!({
            "messaging-slack": {
                "enabled": false
            }
        }))
        .expect("answers");
        assert!(!should_start_setup_tunnel("cloudflared", &disabled_answers));

        assert_eq!(
            extract_tunnel_https_url(
                "cloudflared",
                "INF tunnel running at https://demo.trycloudflare.com"
            ),
            Some("https://demo.trycloudflare.com".to_string())
        );
        assert_eq!(
            extract_tunnel_https_url("ngrok", "url=https://demo.ngrok-free.app latency=1ms"),
            Some("https://demo.ngrok-free.app".to_string())
        );
        assert_eq!(
            extract_tunnel_https_url(
                "cloudflared",
                "Terms: https://www.cloudflare.com/website-terms tunnel https://demo.trycloudflare.com"
            ),
            Some("https://demo.trycloudflare.com".to_string())
        );
        assert_eq!(
            extract_tunnel_https_url(
                "cloudflared",
                "Terms: https://www.cloudflare.com/website-terms"
            ),
            None
        );
        assert_eq!(
            extract_tunnel_https_url(
                "ngrok",
                "Forwarding https://demo.ngrok-free.app -> http://127.0.0.1:1234"
            ),
            Some("https://demo.ngrok-free.app".to_string())
        );
    }

    #[test]
    fn should_start_tunnel_ngrok_mode() {
        let answers = serde_json::from_value::<JsonMap<String, Value>>(json!({
            "messaging-slack": {}
        }))
        .expect("answers");
        assert!(should_start_setup_tunnel("ngrok", &answers));
    }

    #[test]
    fn should_start_tunnel_non_object_value_ignored() {
        let answers = serde_json::from_value::<JsonMap<String, Value>>(json!({
            "messaging-slack": "not-an-object"
        }))
        .expect("answers");
        assert!(!should_start_setup_tunnel("cloudflared", &answers));
    }

    #[test]
    fn should_start_tunnel_whitespace_only_url_needs_tunnel() {
        let answers = serde_json::from_value::<JsonMap<String, Value>>(json!({
            "messaging-slack": {
                "public_base_url": "   "
            }
        }))
        .expect("answers");
        assert!(should_start_setup_tunnel("cloudflared", &answers));
    }

    #[test]
    fn should_start_tunnel_http_url_needs_tunnel() {
        let answers = serde_json::from_value::<JsonMap<String, Value>>(json!({
            "messaging-slack": {
                "public_base_url": "http://127.0.0.1:8080"
            }
        }))
        .expect("answers");
        assert!(should_start_setup_tunnel("cloudflared", &answers));
    }

    #[test]
    fn should_start_tunnel_stale_ngrok_url_needs_tunnel() {
        let answers = serde_json::from_value::<JsonMap<String, Value>>(json!({
            "messaging-telegram": {
                "public_base_url": "https://stale.ngrok-free.app"
            }
        }))
        .expect("answers");
        assert!(should_start_setup_tunnel("ngrok", &answers));
    }

    #[test]
    fn should_start_tunnel_mixed_providers_one_needs_tunnel() {
        let answers = serde_json::from_value::<JsonMap<String, Value>>(json!({
            "messaging-teams": {
                "public_base_url": "https://stable.example.com"
            },
            "messaging-slack": {
                "public_base_url": "http://localhost:3000"
            }
        }))
        .expect("answers");
        // One provider has http, so tunnel is needed.
        assert!(should_start_setup_tunnel("cloudflared", &answers));
    }

    #[test]
    fn should_start_tunnel_all_have_stable_https() {
        let answers = serde_json::from_value::<JsonMap<String, Value>>(json!({
            "messaging-teams": {
                "public_base_url": "https://stable.example.com"
            },
            "messaging-slack": {
                "public_base_url": "https://prod.example.com"
            }
        }))
        .expect("answers");
        assert!(!should_start_setup_tunnel("cloudflared", &answers));
    }

    #[test]
    fn should_start_tunnel_empty_answers_map() {
        let answers = JsonMap::new();
        // No providers at all: no one needs a tunnel.
        assert!(!should_start_setup_tunnel("cloudflared", &answers));
    }

    // ---- extract_https_urls ----

    #[test]
    fn extract_https_urls_empty_line() {
        assert!(extract_https_urls("").is_empty());
    }

    #[test]
    fn extract_https_urls_no_urls() {
        assert!(extract_https_urls("just some text without urls").is_empty());
    }

    #[test]
    fn extract_https_urls_single_url() {
        let urls = extract_https_urls("visit https://example.com now");
        assert_eq!(urls, vec!["https://example.com"]);
    }

    #[test]
    fn extract_https_urls_trailing_slash_stripped() {
        let urls = extract_https_urls("https://example.com/");
        assert_eq!(urls, vec!["https://example.com"]);
    }

    #[test]
    fn extract_https_urls_multiple_urls() {
        let urls =
            extract_https_urls("first https://one.example.com then https://two.example.com end");
        assert_eq!(
            urls,
            vec!["https://one.example.com", "https://two.example.com"]
        );
    }

    #[test]
    fn extract_https_urls_quoted_terminators() {
        let urls = extract_https_urls(r#""https://quoted.example.com""#);
        assert_eq!(urls, vec!["https://quoted.example.com"]);

        let urls = extract_https_urls("'https://single-quoted.example.com'");
        assert_eq!(urls, vec!["https://single-quoted.example.com"]);
    }

    #[test]
    fn extract_https_urls_angle_bracket_terminators() {
        let urls = extract_https_urls("<https://bracketed.example.com>");
        assert_eq!(urls, vec!["https://bracketed.example.com"]);
    }

    #[test]
    fn extract_https_urls_comma_terminator() {
        let urls = extract_https_urls("https://a.com,https://b.com");
        assert_eq!(urls, vec!["https://a.com", "https://b.com"]);
    }

    #[test]
    fn extract_https_urls_paren_terminator() {
        let urls = extract_https_urls("(https://paren.example.com)");
        assert_eq!(urls, vec!["https://paren.example.com"]);
    }

    #[test]
    fn extract_https_urls_with_path() {
        let urls = extract_https_urls("at https://example.com/path/to/thing done");
        assert_eq!(urls, vec!["https://example.com/path/to/thing"]);
    }

    #[test]
    fn extract_https_urls_ignores_http() {
        let urls = extract_https_urls("http://not-extracted.com https://extracted.com");
        assert_eq!(urls, vec!["https://extracted.com"]);
    }

    // ---- tunnel_url_matches_mode ----

    #[test]
    fn tunnel_url_matches_cloudflared_exact_host() {
        assert!(tunnel_url_matches_mode(
            "cloudflared",
            "https://trycloudflare.com"
        ));
    }

    #[test]
    fn tunnel_url_matches_cloudflared_subdomain() {
        assert!(tunnel_url_matches_mode(
            "cloudflared",
            "https://abc-def.trycloudflare.com"
        ));
    }

    #[test]
    fn tunnel_url_rejects_cloudflared_wrong_domain() {
        assert!(!tunnel_url_matches_mode(
            "cloudflared",
            "https://example.com"
        ));
    }

    #[test]
    fn tunnel_url_matches_ngrok_free_app() {
        assert!(tunnel_url_matches_mode(
            "ngrok",
            "https://abc123.ngrok-free.app"
        ));
    }

    #[test]
    fn tunnel_url_matches_ngrok_io() {
        assert!(tunnel_url_matches_mode("ngrok", "https://abc123.ngrok.io"));
    }

    #[test]
    fn tunnel_url_rejects_ngrok_wrong_domain() {
        assert!(!tunnel_url_matches_mode("ngrok", "https://example.com"));
    }

    #[test]
    fn tunnel_url_rejects_unknown_mode() {
        assert!(!tunnel_url_matches_mode(
            "unknown",
            "https://demo.trycloudflare.com"
        ));
    }

    #[test]
    fn tunnel_url_rejects_http_scheme() {
        assert!(!tunnel_url_matches_mode(
            "cloudflared",
            "http://demo.trycloudflare.com"
        ));
    }

    #[test]
    fn tunnel_url_rejects_malformed_url() {
        assert!(!tunnel_url_matches_mode("cloudflared", "not a url"));
    }

    // ---- extract_tunnel_https_url (additional edge cases) ----

    #[test]
    fn extract_tunnel_url_empty_line() {
        assert_eq!(extract_tunnel_https_url("cloudflared", ""), None);
    }

    #[test]
    fn extract_tunnel_url_no_matching_domain() {
        assert_eq!(
            extract_tunnel_https_url("cloudflared", "https://unrelated.example.com"),
            None
        );
    }

    #[test]
    fn extract_tunnel_url_ngrok_io_legacy() {
        assert_eq!(
            extract_tunnel_https_url("ngrok", "tunnel at https://abc.ngrok.io"),
            Some("https://abc.ngrok.io".to_string())
        );
    }

    // ---- is_ephemeral_tunnel_url (additional edge cases) ----

    #[test]
    fn ephemeral_url_http_not_ephemeral() {
        assert!(!is_ephemeral_tunnel_url("http://demo.trycloudflare.com"));
    }

    #[test]
    fn ephemeral_url_trycloudflare_exact_root() {
        assert!(is_ephemeral_tunnel_url("https://trycloudflare.com"));
    }

    #[test]
    fn ephemeral_url_ngrok_io_subdomain() {
        assert!(is_ephemeral_tunnel_url("https://deep.sub.ngrok.io/path"));
    }

    #[test]
    fn ephemeral_url_malformed_not_ephemeral() {
        assert!(!is_ephemeral_tunnel_url("not-a-url"));
    }

    #[test]
    fn ephemeral_url_mixed_case() {
        assert!(is_ephemeral_tunnel_url("https://DEMO.TryCloudflare.COM"));
    }

    // ---- managed-tunnel (gtunnel) URLs are refreshable too ----
    //
    // The host is stable but the tunnel id in the first path segment is not, so a
    // managed URL from an earlier run must be re-pointed rather than preserved as
    // if an operator had supplied it.

    #[test]
    fn managed_worker_url_is_refreshable_on_the_default_worker_host() {
        // Env-free: falls back to DEFAULT_GTUNNEL_WORKER_BASE_URL.
        assert!(is_ephemeral_tunnel_url(&format!(
            "{DEFAULT_GTUNNEL_WORKER_BASE_URL}/default"
        )));
        assert!(is_ephemeral_tunnel_url(&format!(
            "{DEFAULT_GTUNNEL_WORKER_BASE_URL}/default-default"
        )));
        // Bare host, no id segment at all.
        assert!(is_ephemeral_tunnel_url(DEFAULT_GTUNNEL_WORKER_BASE_URL));
    }

    #[test]
    fn managed_worker_url_is_refreshable_on_a_self_hosted_worker_host() {
        // Worker base passed explicitly rather than via env: this crate has no
        // temp-env dev-dependency, and mutating process env would race the other
        // tests in this binary.
        assert!(is_ephemeral_tunnel_url_for_worker_base(
            "https://tunnel.acme.example/default",
            "https://tunnel.acme.example"
        ));
        // Host comparison is case-insensitive and ignores the base's trailing path.
        assert!(is_ephemeral_tunnel_url_for_worker_base(
            "https://Tunnel.ACME.example/demo",
            "https://tunnel.acme.example/"
        ));
    }

    #[test]
    fn genuine_operator_url_is_still_preserved() {
        // The whole point of the predicate: a permanent operator-supplied ingress
        // must never be replaced by a tunnel URL.
        assert!(!is_ephemeral_tunnel_url("https://hooks.example.com"));
        assert!(!is_ephemeral_tunnel_url(
            "https://hooks.example.com/webhooks"
        ));
        // And it stays preserved when a self-hosted Worker is configured.
        assert!(!is_ephemeral_tunnel_url_for_worker_base(
            "https://hooks.example.com",
            "https://tunnel.acme.example"
        ));
    }

    #[test]
    fn managed_worker_host_matches_the_configured_base_only() {
        // Documents the chosen semantics: the predicate keys on the CONFIGURED
        // Worker base, so with a self-hosted Worker a leftover URL on the default
        // Greentic Worker host is not claimed. Kept deliberately narrow — the
        // configured host is the one this run can actually serve.
        assert!(!is_ephemeral_tunnel_url_for_worker_base(
            &format!("{DEFAULT_GTUNNEL_WORKER_BASE_URL}/default"),
            "https://tunnel.acme.example"
        ));
        // A malformed worker base disables managed matching but must not break
        // the trycloudflare/ngrok arms.
        assert!(!is_ephemeral_tunnel_url_for_worker_base(
            "https://hooks.example.com",
            "not-a-url"
        ));
        assert!(is_ephemeral_tunnel_url_for_worker_base(
            "https://demo.trycloudflare.com",
            "not-a-url"
        ));
    }

    #[test]
    fn inject_replaces_a_stale_managed_url_under_a_different_id() {
        // The live regression: an earlier run configured these providers under the
        // old `default-default` id, the current run serves `default`, and the URL
        // "looked stable" so it was preserved forever — leaving Webex's registered
        // webhook pointing at a path the tunnel no longer serves.
        let stale = format!("{DEFAULT_GTUNNEL_WORKER_BASE_URL}/default-default");
        let current = format!("{DEFAULT_GTUNNEL_WORKER_BASE_URL}/default");
        let mut answers = serde_json::from_value::<JsonMap<String, Value>>(json!({
            "messaging-webex": { "public_base_url": stale },
            "messaging-slack": { "public_base_url": current },
            "messaging-operator": { "public_base_url": "https://hooks.example.com" },
        }))
        .expect("answers");

        inject_setup_public_base_url(&mut answers, &current);

        assert_eq!(
            answers["messaging-webex"]["public_base_url"],
            json!(current),
            "a managed URL under the old id must be re-pointed at the current id"
        );
        assert_eq!(
            answers["messaging-slack"]["public_base_url"],
            json!(current)
        );
        assert_eq!(
            answers["messaging-operator"]["public_base_url"],
            json!("https://hooks.example.com"),
            "a genuine operator URL must survive untouched"
        );
    }

    #[test]
    fn should_start_setup_tunnel_when_only_url_is_a_stale_managed_one() {
        let answers = serde_json::from_value::<JsonMap<String, Value>>(json!({
            "messaging-webex": {
                "public_base_url": format!("{DEFAULT_GTUNNEL_WORKER_BASE_URL}/default-default")
            },
        }))
        .expect("answers");
        assert!(
            should_start_setup_tunnel("gtunnel", &answers),
            "a stale managed URL must not convince setup the tunnel is unnecessary"
        );

        // Contrast: a real operator URL genuinely needs no tunnel.
        let operator = serde_json::from_value::<JsonMap<String, Value>>(json!({
            "messaging-webex": { "public_base_url": "https://hooks.example.com" },
        }))
        .expect("answers");
        assert!(!should_start_setup_tunnel("gtunnel", &operator));
    }

    // ---- inject_setup_public_base_url ----

    #[test]
    fn setup_tunnel_url_overrides_missing_or_non_https_provider_answers() {
        let mut answers = serde_json::from_value::<JsonMap<String, Value>>(json!({
            "messaging-slack": {
                "public_base_url": "http://127.0.0.1:35519",
                "slack_configuration_access_token": "x"
            },
            "messaging-teams": {
                "public_base_url": "https://stable.example.com"
            },
            "messaging-stale-tunnel": {
                "public_base_url": "https://old.trycloudflare.com"
            },
            "messaging-disabled": {
                "enabled": false
            },
            "messaging-webhook": {}
        }))
        .expect("answers");

        inject_setup_public_base_url(&mut answers, "https://setup.trycloudflare.com");

        assert_eq!(
            answers["messaging-slack"]["public_base_url"],
            json!("https://setup.trycloudflare.com")
        );
        assert_eq!(
            answers["messaging-webhook"]["public_base_url"],
            json!("https://setup.trycloudflare.com")
        );
        assert_eq!(answers["messaging-disabled"].get("public_base_url"), None);
        assert_eq!(
            answers["messaging-teams"]["public_base_url"],
            json!("https://stable.example.com")
        );
        assert_eq!(
            answers["messaging-stale-tunnel"]["public_base_url"],
            json!("https://setup.trycloudflare.com")
        );
    }

    #[test]
    fn inject_skips_non_object_values() {
        let mut answers = serde_json::from_value::<JsonMap<String, Value>>(json!({
            "scalar": "not-an-object",
            "array": [1, 2, 3],
            "null": null,
            "provider": { "enabled": true }
        }))
        .expect("answers");

        inject_setup_public_base_url(&mut answers, "https://new.trycloudflare.com");

        // Scalar, array, and null are skipped entirely.
        assert_eq!(answers["scalar"], json!("not-an-object"));
        assert_eq!(answers["array"], json!([1, 2, 3]));
        assert_eq!(answers["null"], json!(null));
        // Enabled object provider gets injected.
        assert_eq!(
            answers["provider"]["public_base_url"],
            json!("https://new.trycloudflare.com")
        );
    }

    #[test]
    fn inject_preserves_ngrok_stale_url() {
        let mut answers = serde_json::from_value::<JsonMap<String, Value>>(json!({
            "messaging-telegram": {
                "public_base_url": "https://old.ngrok-free.app"
            }
        }))
        .expect("answers");

        inject_setup_public_base_url(&mut answers, "https://new.ngrok-free.app");

        assert_eq!(
            answers["messaging-telegram"]["public_base_url"],
            json!("https://new.ngrok-free.app")
        );
    }

    #[test]
    fn inject_whitespace_only_url_gets_replaced() {
        let mut answers = serde_json::from_value::<JsonMap<String, Value>>(json!({
            "messaging-slack": {
                "public_base_url": "   "
            }
        }))
        .expect("answers");

        inject_setup_public_base_url(&mut answers, "https://demo.trycloudflare.com");

        assert_eq!(
            answers["messaging-slack"]["public_base_url"],
            json!("https://demo.trycloudflare.com")
        );
    }

    // ---- detects_ephemeral_tunnel_urls (original test preserved) ----

    #[test]
    fn detects_ephemeral_tunnel_urls() {
        assert!(is_ephemeral_tunnel_url("https://demo.trycloudflare.com"));
        assert!(is_ephemeral_tunnel_url("https://demo.ngrok-free.app"));
        assert!(is_ephemeral_tunnel_url("https://demo.ngrok.io"));
        assert!(!is_ephemeral_tunnel_url("https://runtime.example.com"));
    }

    // ---- platform_executable_name ----

    #[test]
    fn platform_executable_name_returns_name() {
        let name = platform_executable_name("cloudflared");
        if cfg!(windows) {
            assert_eq!(name, "cloudflared.exe");
        } else {
            assert_eq!(name, "cloudflared");
        }
    }

    #[test]
    fn platform_executable_name_ngrok() {
        let name = platform_executable_name("ngrok");
        if cfg!(windows) {
            assert_eq!(name, "ngrok.exe");
        } else {
            assert_eq!(name, "ngrok");
        }
    }

    // ---- executable_exists ----

    #[test]
    fn executable_exists_nonexistent_path() {
        assert!(!executable_exists(Path::new("/nonexistent/path/to/binary")));
    }

    #[test]
    fn executable_exists_regular_file_without_exec() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("not-executable");
        std::fs::write(&file_path, b"data").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o644))
                .expect("chmod");
        }
        assert!(!executable_exists(&file_path));
    }

    #[test]
    fn executable_exists_with_exec_bit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("executable");
        std::fs::write(&file_path, b"#!/bin/sh\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }
        assert!(executable_exists(&file_path));
    }

    #[test]
    fn executable_exists_directory_is_false() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!executable_exists(dir.path()));
    }

    // ---- managed_tunnel_binary_path ----

    #[test]
    fn managed_binary_path_contains_binary_name() {
        // Regardless of env, the filename portion should contain the binary name.
        let path = managed_tunnel_binary_path("cloudflared");
        let file_name = path.file_name().expect("has file name");
        assert!(
            file_name.to_str().expect("utf8").contains("cloudflared"),
            "expected cloudflared in path, got {path:?}"
        );
    }

    #[test]
    fn managed_binary_path_ngrok() {
        let path = managed_tunnel_binary_path("ngrok");
        let file_name = path.file_name().expect("has file name");
        assert!(
            file_name.to_str().expect("utf8").contains("ngrok"),
            "expected ngrok in path, got {path:?}"
        );
    }

    // ---- cloudflared_release_asset ----

    #[test]
    fn cloudflared_release_asset_returns_some_on_supported_platform() {
        let asset = cloudflared_release_asset();
        // We're running on a supported CI/dev platform (linux x86_64 or aarch64).
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "x86_64") => assert_eq!(asset, Some("cloudflared-linux-amd64")),
            ("linux", "aarch64") => assert_eq!(asset, Some("cloudflared-linux-arm64")),
            ("macos", "aarch64") => {
                assert_eq!(asset, Some("cloudflared-darwin-arm64.tgz"))
            }
            ("macos", "x86_64") => {
                assert_eq!(asset, Some("cloudflared-darwin-amd64.tgz"))
            }
            _ => {
                // On unsupported platforms, Some or None is fine.
            }
        }
    }

    // ---- resolve_tunnel_binary error branch ----

    #[test]
    fn resolve_tunnel_binary_unsupported_mode() {
        let err = resolve_tunnel_binary("unknown").unwrap_err();
        assert!(
            err.to_string().contains("unsupported"),
            "expected 'unsupported' in error: {err}"
        );
    }

    // ---- resolve_path_binary ----

    #[test]
    fn resolve_path_binary_finds_existing() {
        // "sh" should exist on PATH on any POSIX system.
        if cfg!(unix) {
            let result = resolve_path_binary("sh");
            assert!(result.is_some(), "sh should be found on PATH");
        }
    }

    #[test]
    fn resolve_path_binary_missing_returns_none() {
        let result = resolve_path_binary("nonexistent-binary-xyz-12345");
        assert!(result.is_none());
    }

    // ---- start_setup_tunnel error branch ----

    #[test]
    fn start_setup_tunnel_unsupported_mode() {
        let result = start_setup_tunnel("unknown", "http://127.0.0.1:8080", None);
        assert!(result.is_err());
        let err = result.err().expect("should be Err");
        assert!(
            err.to_string().contains("unsupported"),
            "expected 'unsupported' in error: {err}"
        );
    }

    // ---- SetupTunnel struct / is_running with no child ----

    #[test]
    fn setup_tunnel_no_child_reports_running() {
        // A reused shared-record tunnel has no child process.
        let mut tunnel = SetupTunnel {
            mode: "cloudflared".to_string(),
            local_base_url: "http://127.0.0.1:8080".to_string(),
            public_base_url: "https://demo.trycloudflare.com".to_string(),
            child: None,
            kill_on_drop: false,
        };
        // No child: is_running returns true (liveness is external).
        assert!(tunnel.is_running());
    }

    #[test]
    fn setup_tunnel_drop_no_child_no_kill() {
        // Dropping with no child and kill_on_drop false should not panic.
        let tunnel = SetupTunnel {
            mode: "cloudflared".to_string(),
            local_base_url: "http://127.0.0.1:8080".to_string(),
            public_base_url: "https://demo.trycloudflare.com".to_string(),
            child: None,
            kill_on_drop: false,
        };
        drop(tunnel);
    }

    #[test]
    fn setup_tunnel_drop_with_kill_on_drop_false() {
        // Even with a finished child, kill_on_drop false means Drop does nothing.
        let child = std::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        let tunnel = SetupTunnel {
            mode: "ngrok".to_string(),
            local_base_url: "http://127.0.0.1:9090".to_string(),
            public_base_url: "https://demo.ngrok-free.app".to_string(),
            child: Some(child),
            kill_on_drop: false,
        };
        drop(tunnel);
    }

    #[test]
    fn setup_tunnel_drop_with_kill_on_drop_true() {
        // With kill_on_drop true, Drop kills and waits.
        let child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn sleep");
        let tunnel = SetupTunnel {
            mode: "ngrok".to_string(),
            local_base_url: "http://127.0.0.1:9091".to_string(),
            public_base_url: "https://demo.ngrok-free.app".to_string(),
            child: Some(child),
            kill_on_drop: true,
        };
        drop(tunnel);
    }

    #[test]
    fn setup_tunnel_is_running_with_finished_child() {
        let child = std::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        let mut tunnel = SetupTunnel {
            mode: "ngrok".to_string(),
            local_base_url: "http://127.0.0.1:9092".to_string(),
            public_base_url: "https://demo.ngrok-free.app".to_string(),
            child: Some(child),
            kill_on_drop: false,
        };
        // Wait for the child to finish.
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!tunnel.is_running());
    }

    #[test]
    fn setup_tunnel_is_running_with_alive_child() {
        let child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn sleep");
        let mut tunnel = SetupTunnel {
            mode: "ngrok".to_string(),
            local_base_url: "http://127.0.0.1:9093".to_string(),
            public_base_url: "https://demo.ngrok-free.app".to_string(),
            child: Some(child),
            kill_on_drop: true,
        };
        assert!(tunnel.is_running());
        // Clean up via kill_on_drop.
    }

    // ---- finalize_installed_binary ----

    #[test]
    fn finalize_installed_binary_renames_and_sets_permissions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let temp = dir.path().join("temp-binary");
        let target = dir.path().join("final-binary");
        std::fs::write(&temp, b"fake binary content").expect("write");

        finalize_installed_binary(&temp, &target).expect("finalize");

        assert!(!temp.exists(), "temp file should be renamed away");
        assert!(target.exists(), "target should exist");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&target)
                .expect("meta")
                .permissions()
                .mode();
            assert_ne!(mode & 0o111, 0, "target should be executable");
        }
    }

    // ---- spawn_tunnel_log_reader ----

    #[test]
    fn spawn_log_reader_sends_lines() {
        let input = b"line one\nline two\nline three\n";
        let cursor = std::io::Cursor::new(input.to_vec());
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        spawn_tunnel_log_reader(cursor, tx);

        let mut lines = Vec::new();
        while let Ok(line) = rx.recv_timeout(std::time::Duration::from_secs(1)) {
            lines.push(line);
        }
        assert_eq!(lines, vec!["line one", "line two", "line three"]);
    }

    #[test]
    fn spawn_log_reader_empty_input() {
        let cursor = std::io::Cursor::new(Vec::new());
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        spawn_tunnel_log_reader(cursor, tx);

        // Should produce no lines and disconnect promptly.
        let result = rx.recv_timeout(std::time::Duration::from_millis(500));
        assert!(result.is_err());
    }
}
