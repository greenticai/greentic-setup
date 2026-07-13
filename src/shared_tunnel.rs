//! Machine-wide shared quick-tunnel record, file-protocol compatible with
//! greentic-start (`greentic-start/src/tunnel_state.rs`).
//!
//! A quick tunnel fronts exactly one local port, so one tunnel per
//! (machine, port) is both necessary and sufficient. Setup and the runtime
//! both honour a shared on-disk record instead of each spawning (and
//! previously killing) their own cloudflared:
//!
//! - pidfile:   `<root>/state/pids/shared.cloudflared-<port>/cloudflared.pid`
//! - URL cache: `<root>/state/runtime/shared.cloudflared-<port>/public_base_url.txt`
//! - log:       `<root>/logs/shared.cloudflared-<port>/cloudflared.log`
//! - spawn lock: `<root>/state/cloudflared-<port>.lock`
//!
//! `<root>` is `~/.greentic/tunnel` (override: `GREENTIC_TUNNEL_STATE_DIR`).
//! greentic-setup does not depend on greentic-start, so this module
//! implements the same protocol independently; changing these paths is a
//! cross-repo protocol change. Only processes recorded here are ever
//! terminated — never by name.

use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A lock file untouched for this long belongs to a crashed process and may
/// be reclaimed. Spawn + URL discovery hold the lock for well under a minute.
const LOCK_STALE_AFTER: Duration = Duration::from_secs(120);

/// How long after spawn a quick tunnel may stay absent from public DNS before
/// it counts as dead rather than propagating. Fresh `*.trycloudflare.com`
/// hostnames appear in public DNS within a couple of minutes; when a quick
/// tunnel dies, Cloudflare removes the hostname from DNS entirely — so a
/// hostname still unresolvable this long after the record was written is a
/// dead tunnel, not a slow one. (This also explains why the HTTP 530
/// "binding lost" proof never arrives for dead tunnels: with no DNS record
/// there is nothing to return the 530.)
const DNS_WARMUP_DEADLINE: Duration = Duration::from_secs(10 * 60);

/// On-disk paths of the shared cloudflared record for one local port.
#[derive(Clone, Debug)]
pub struct SharedTunnelPaths {
    pub pid_path: PathBuf,
    pub url_path: PathBuf,
    pub log_path: PathBuf,
    pub lock_path: PathBuf,
}

fn tunnel_state_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("GREENTIC_TUNNEL_STATE_DIR") {
        return PathBuf::from(dir);
    }
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".greentic")
        .join("tunnel")
}

pub fn shared_tunnel_paths(port: u16) -> SharedTunnelPaths {
    shared_tunnel_paths_at(&tunnel_state_root(), port)
}

pub(crate) fn shared_tunnel_paths_at(root: &Path, port: u16) -> SharedTunnelPaths {
    let state = root.join("state");
    let key = format!("shared.cloudflared-{port}");
    SharedTunnelPaths {
        pid_path: state.join("pids").join(&key).join("cloudflared.pid"),
        url_path: state.join("runtime").join(&key).join("public_base_url.txt"),
        log_path: root.join("logs").join(&key).join("cloudflared.log"),
        lock_path: state.join(format!("cloudflared-{port}.lock")),
    }
}

/// Local port a tunnel for `local_base_url` would be keyed on.
pub fn local_port_from_base_url(local_base_url: &str) -> Option<u16> {
    url::Url::parse(local_base_url)
        .ok()
        .and_then(|url| url.port_or_known_default())
}

/// Read the recorded (pid, url) pair; either half may be absent.
pub fn read_record(paths: &SharedTunnelPaths) -> (Option<u32>, Option<String>) {
    let pid = std::fs::read_to_string(&paths.pid_path)
        .ok()
        .and_then(|contents| contents.trim().parse().ok());
    let url = std::fs::read_to_string(&paths.url_path)
        .ok()
        .map(|contents| contents.trim().to_string())
        .filter(|value| value.starts_with("https://"));
    (pid, url)
}

/// Publish a spawned tunnel into the shared record so other Greentic
/// processes (greentic-start in particular) reuse it instead of respawning.
pub fn write_record(paths: &SharedTunnelPaths, pid: u32, url: &str) -> anyhow::Result<()> {
    write_atomic(&paths.pid_path, pid.to_string().as_bytes())?;
    write_atomic(&paths.url_path, url.as_bytes())?;
    // Mirror the URL into the shared log: greentic-start falls back to
    // scanning it for a `*.trycloudflare.com` URL when the url file is gone.
    if let Some(parent) = paths.log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.log_path)?;
    writeln!(log, "greentic-setup: quick tunnel running at {url}")?;
    Ok(())
}

pub fn clear_record(paths: &SharedTunnelPaths) {
    let _ = std::fs::remove_file(&paths.pid_path);
    let _ = std::fs::remove_file(&paths.url_path);
}

fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path {} has no parent", path.display()))?;
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Whether `pid` currently belongs to a cloudflared process. Guards against
/// PID reuse: a recorded pid recycled by the OS onto an unrelated process
/// must neither count as tunnel liveness nor be terminated.
fn process_is_cloudflared(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .output()
            .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("cloudflared"))
    }
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .is_ok_and(|out| {
                String::from_utf8_lossy(&out.stdout)
                    .to_ascii_lowercase()
                    .contains("cloudflared")
            })
    }
}

/// Terminate the recorded process. Only ever called with a PID read from the
/// shared record — ownership is proven by the record, never by process name —
/// and even then only when the pid still runs cloudflared, so a recycled pid
/// cannot get an unrelated process killed.
pub fn terminate_recorded_pid(pid: u32) {
    if !process_is_cloudflared(pid) {
        eprintln!("Shared tunnel: recorded pid {pid} is not a cloudflared process — not killing");
        return;
    }
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
        std::thread::sleep(Duration::from_millis(500));
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status();
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .status();
    }
}

/// What a single HEAD probe of a recorded tunnel URL tells us.
enum ProbeOutcome {
    /// The edge routed the request to the origin — 2xx/3xx, or any origin error
    /// status other than 530 (a 400/404 from the origin still proves routing).
    /// The tunnel serves end to end.
    Serving,
    /// Cloudflare's 530 "tunnel is down" page: the edge has no origin tunnel
    /// bound to this hostname. The binding is genuinely gone.
    EdgeDown,
    /// Transport/DNS failure. Inconclusive — the tunnel may be perfectly healthy
    /// and only unreachable from *this* host (see `classify_recorded_tunnel`).
    Unreachable,
}

/// Single HEAD probe against `url` using this host's resolver.
fn head_probe(url: &str) -> ProbeOutcome {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(4)))
        .build()
        .new_agent();
    match agent.head(url).call() {
        Ok(_) => ProbeOutcome::Serving,
        Err(ureq::Error::StatusCode(530)) => ProbeOutcome::EdgeDown,
        Err(ureq::Error::StatusCode(_)) => ProbeOutcome::Serving,
        Err(_) => ProbeOutcome::Unreachable,
    }
}

/// What public DNS says about a tunnel hostname.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PublicDnsVerdict {
    /// Published — remote parties (Slack, Teams, the Bot Framework, ...) can
    /// resolve it, which is what actually matters for a tunnel fronting
    /// provider webhooks.
    Published(IpAddr),
    /// At least one public resolver answered and the name has no A record.
    /// For a quick tunnel this is evidence of death: Cloudflare removes the
    /// hostname from DNS when the tunnel goes away.
    Absent,
    /// No public resolver could be reached — says nothing about the tunnel
    /// (e.g. a network that blocks DoH endpoints). Must not count as proof.
    Unknown,
}

/// Query one DoH JSON endpoint for `host`'s A record.
/// `Some(Some(ip))` — published; `Some(None)` — the resolver answered and the
/// name is absent; `None` — the resolver itself was unreachable.
fn query_doh_a_record(endpoint: &str, host: &str) -> Option<Option<IpAddr>> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(3)))
        .build()
        .new_agent();
    let query = format!("{endpoint}?name={host}&type=A");
    let mut response = agent
        .get(&query)
        .header("accept", "application/dns-json")
        .call()
        .ok()?;
    let body: serde_json::Value = response.body_mut().read_json().ok()?;
    // A parsed DNS answer (any Status, e.g. NXDOMAIN) is an authoritative
    // reply; require the Status field so an unrelated JSON body (captive
    // portal, block page) does not count as one.
    body.get("Status")?.as_u64()?;
    let ip = body
        .get("Answer")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        // type 1 = A record; CNAME chain entries (type 5) also appear here.
        .filter(|answer| answer.get("type").and_then(serde_json::Value::as_u64) == Some(1))
        .find_map(|answer| answer.get("data")?.as_str()?.parse().ok());
    Some(ip)
}

/// Resolve `host` via public DNS-over-HTTPS resolvers, addressed by IP
/// literal so it works even when this host's resolver is blind to the zone.
/// Two independent resolvers, so one blocked or flaky endpoint cannot turn
/// into a false "absent" verdict that gets a healthy tunnel killed.
fn resolve_via_public_dns(host: &str) -> PublicDnsVerdict {
    let mut any_answered = false;
    for endpoint in ["https://1.1.1.1/dns-query", "https://8.8.8.8/resolve"] {
        match query_doh_a_record(endpoint, host) {
            Some(Some(ip)) => return PublicDnsVerdict::Published(ip),
            Some(None) => any_answered = true,
            None => {}
        }
    }
    if any_answered {
        PublicDnsVerdict::Absent
    } else {
        PublicDnsVerdict::Unknown
    }
}

/// Whether process `pid` is currently alive. Uses a `kill -0` existence probe
/// (delivers no signal) — consistent with `terminate_recorded_pid`, and avoids
/// pulling in a `libc`/`nix` dependency just for this.
pub fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|status| status.success())
    }
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()))
    }
}

/// Whether the tunnel log shows cloudflared *currently* holds an edge
/// connection — proof the tunnel came up at Cloudflare's edge even before DNS
/// propagates. Registration must postdate the last unregistration: a
/// "Registered tunnel connection" line stays in the log forever, so its mere
/// presence says nothing about a tunnel that has since lost the edge.
/// (Case matters: "Unregistered tunnel connection" does not contain the
/// capital-R needle, so the two searches cannot cross-match.)
fn log_shows_registered_connection(log_path: &Path) -> bool {
    std::fs::read_to_string(log_path).is_ok_and(|contents| {
        match (
            contents.rfind("Registered tunnel connection"),
            contents.rfind("Unregistered tunnel connection"),
        ) {
            (Some(registered), Some(unregistered)) => registered > unregistered,
            (Some(_), None) => true,
            (None, _) => false,
        }
    })
}

/// Age of the shared record, from the url file's mtime — written once at
/// spawn (reuse never rewrites it), so this is time since the tunnel was
/// minted. `None` when the age cannot be established.
fn record_age(paths: &SharedTunnelPaths) -> Option<Duration> {
    std::fs::metadata(&paths.url_path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
}

/// Host component of `url`, for a DNS lookup.
fn url_host(url: &str) -> Option<String> {
    url::Url::parse(url).ok()?.host_str().map(str::to_string)
}

/// Verdict on whether a recorded tunnel should be reused or replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordedTunnelState {
    /// Reachable now — directly, or published in public DNS. Reuse it.
    Serving,
    /// The cloudflared process is alive and registered with the edge, but the
    /// hostname has not propagated into public DNS yet. Reuse and wait: a fresh
    /// quick tunnel can take minutes to appear in DNS, and respawning would only
    /// reset that clock and orphan the URL already handed to providers earlier
    /// in this setup run. Only a *recent* record qualifies — past
    /// [`DNS_WARMUP_DEADLINE`] an unresolvable hostname is dead, not warming.
    WarmingUp,
    /// No usable tunnel — the process is gone, the edge returned 530 (binding
    /// lost), it never registered (or lost its last edge connection), or its
    /// hostname stayed out of public DNS past the warm-up deadline. Replace it.
    Down,
}

/// Decide whether the recorded tunnel (`pid`, `url`) is still usable.
///
/// The reuse decision deliberately does **not** hinge on a plain HTTP probe
/// from this host. Freshly-minted `*.trycloudflare.com` hostnames land in the
/// OS resolver's negative-DNS cache (30-min TTL) and lag public-DNS propagation
/// by minutes, so a healthy tunnel probes as "dead" locally for a while.
/// Tearing it down on that signal is exactly what makes setup mint a new URL on
/// every wizard step and strand provider webhooks on a now-dead hostname. So we
/// escalate through increasingly authoritative signals and only return `Down`
/// on positive proof: a 530, a dead (or recycled) pid, a lost edge
/// registration, or absence from public DNS past [`DNS_WARMUP_DEADLINE`] —
/// the last one matters because a dead quick tunnel's hostname leaves DNS
/// entirely, so the 530 proof can never arrive for it. Each branch logs what
/// it saw, to keep this debuggable.
pub fn classify_recorded_tunnel(
    paths: &SharedTunnelPaths,
    pid: Option<u32>,
    url: &str,
) -> RecordedTunnelState {
    // 1. Direct probe. A routed response proves it serves; a 530 proves the
    //    edge binding is gone. Anything else is inconclusive from here.
    match head_probe(url) {
        ProbeOutcome::Serving => {
            eprintln!("Shared tunnel {url}: reachable directly — reusing (Serving)");
            return RecordedTunnelState::Serving;
        }
        ProbeOutcome::EdgeDown => {
            eprintln!("Shared tunnel {url}: edge returned 530 (binding lost) — replacing (Down)");
            return RecordedTunnelState::Down;
        }
        ProbeOutcome::Unreachable => {
            eprintln!(
                "Shared tunnel {url}: not reachable via the local resolver; checking public DNS"
            );
        }
    }

    // 2. The local resolver may just be blind. Ask public DNS directly: if the
    //    hostname resolves there, remote providers can reach it even though we
    //    cannot, so it is serving for the parties that matter.
    let dns = match url_host(url) {
        Some(host) => resolve_via_public_dns(&host),
        None => PublicDnsVerdict::Unknown,
    };
    match dns {
        PublicDnsVerdict::Published(ip) => {
            eprintln!(
                "Shared tunnel {url}: unreachable locally but published in public DNS ({ip}) \
                 — the OS resolver has a stale negative cache; remote providers resolve it \
                 fine — reusing (Serving)"
            );
            return RecordedTunnelState::Serving;
        }
        PublicDnsVerdict::Absent => {
            eprintln!("Shared tunnel {url}: not published in public DNS (1.1.1.1/8.8.8.8)");
        }
        PublicDnsVerdict::Unknown => {
            eprintln!(
                "Shared tunnel {url}: no public DNS resolver reachable — cannot tell whether \
                 the hostname is published"
            );
        }
    }

    // 3. Not reachable from anywhere yet. Decide from local evidence whether
    //    this is a fresh tunnel mid-propagation (reuse and wait) or a dead one
    //    (its hostname will never come back — let it go).
    let running = pid.is_some_and(|pid| process_alive(pid) && process_is_cloudflared(pid));
    let registered = log_shows_registered_connection(&paths.log_path);
    let age = record_age(paths);
    eprintln!(
        "Shared tunnel {url}: local pid={pid:?} alive-cloudflared={running}, \
         edge-registered={registered}, record-age={age:?}, dns={dns:?}"
    );
    classify_local_evidence(
        url,
        running,
        registered,
        age,
        dns == PublicDnsVerdict::Absent,
    )
}

/// Step-3 verdict from local evidence alone, once probes and public DNS have
/// both come back empty. Separate from [`classify_recorded_tunnel`] so the
/// decision table is unit-testable without network access. `dns_absent` is
/// true only when a public resolver positively answered that the hostname has
/// no record — an unreachable resolver is not evidence.
fn classify_local_evidence(
    url: &str,
    running: bool,
    registered: bool,
    age: Option<Duration>,
    dns_absent: bool,
) -> RecordedTunnelState {
    if !(running && registered) {
        eprintln!("Shared tunnel {url}: no live/registered cloudflared — replacing (Down)");
        return RecordedTunnelState::Down;
    }
    // Unknown age gives no proof of death — keep the reuse bias.
    let past_deadline = age.is_some_and(|age| age > DNS_WARMUP_DEADLINE);
    if past_deadline && dns_absent {
        eprintln!(
            "Shared tunnel {url}: cloudflared is alive but the hostname is confirmed absent \
             from public DNS {}s after spawn — a healthy quick tunnel propagates within \
             minutes, and dead ones drop out of DNS entirely; letting this one go — \
             replacing (Down)",
            age.map(|age| age.as_secs()).unwrap_or_default()
        );
        RecordedTunnelState::Down
    } else {
        eprintln!(
            "Shared tunnel {url}: cloudflared alive and registered with the edge — still \
             propagating into public DNS; reusing rather than minting a new URL and orphaning \
             provider webhooks (WarmingUp)"
        );
        RecordedTunnelState::WarmingUp
    }
}

/// Advisory file lock guarding the check-then-spawn critical section: exists
/// while held, reclaimed when stale. Dropping releases it.
#[derive(Debug)]
pub struct TunnelLock {
    path: PathBuf,
}

impl TunnelLock {
    pub fn acquire(path: &Path, wait: Duration) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let deadline = Instant::now() + wait;
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(mut file) => {
                    let _ = write!(file, "{}", std::process::id());
                    return Ok(Self {
                        path: path.to_path_buf(),
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(path) {
                        let _ = std::fs::remove_file(path);
                        continue;
                    }
                    if Instant::now() >= deadline {
                        return Err(anyhow::anyhow!(
                            "timed out waiting for tunnel spawn lock {} (remove it if no other greentic process is starting a tunnel)",
                            path.display()
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(err) => return Err(err.into()),
            }
        }
    }
}

fn lock_is_stale(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age > LOCK_STALE_AFTER)
}

impl Drop for TunnelLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn shared_paths_match_greentic_start_protocol() {
        let paths = shared_tunnel_paths_at(Path::new("/tunnel-root"), 8443);
        assert_eq!(
            paths.pid_path,
            Path::new("/tunnel-root/state/pids/shared.cloudflared-8443/cloudflared.pid")
        );
        assert_eq!(
            paths.url_path,
            Path::new("/tunnel-root/state/runtime/shared.cloudflared-8443/public_base_url.txt")
        );
        assert_eq!(
            paths.log_path,
            Path::new("/tunnel-root/logs/shared.cloudflared-8443/cloudflared.log")
        );
        assert_eq!(
            paths.lock_path,
            Path::new("/tunnel-root/state/cloudflared-8443.lock")
        );
    }

    #[test]
    fn record_roundtrip_and_clear() {
        let dir = tempdir().expect("tempdir");
        let paths = shared_tunnel_paths_at(dir.path(), 8080);

        assert_eq!(read_record(&paths), (None, None));

        write_record(&paths, 4242, "https://demo.trycloudflare.com").expect("write record");
        assert_eq!(
            read_record(&paths),
            (
                Some(4242),
                Some("https://demo.trycloudflare.com".to_string())
            )
        );
        let log = std::fs::read_to_string(&paths.log_path).expect("log");
        assert!(log.contains("https://demo.trycloudflare.com"));

        clear_record(&paths);
        assert_eq!(read_record(&paths), (None, None));
    }

    #[test]
    fn local_port_parses_explicit_and_default_ports() {
        assert_eq!(
            local_port_from_base_url("http://127.0.0.1:35519"),
            Some(35519)
        );
        assert_eq!(local_port_from_base_url("http://127.0.0.1"), Some(80));
        assert_eq!(local_port_from_base_url("not a url"), None);
    }

    #[cfg(unix)]
    #[test]
    fn process_alive_true_for_self_false_for_reaped() {
        assert!(process_alive(std::process::id()));
        // A child we've spawned and reaped is no longer alive.
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        let pid = child.id();
        child.wait().expect("reap true");
        assert!(!process_alive(pid));
    }

    #[test]
    fn registration_detected_only_when_logged() {
        let dir = tempdir().expect("tempdir");
        let log = dir.path().join("cloudflared.log");
        assert!(
            !log_shows_registered_connection(&log),
            "missing file → false"
        );
        std::fs::write(&log, "INF Starting metrics server\n").expect("write");
        assert!(
            !log_shows_registered_connection(&log),
            "no registration line → false"
        );
        std::fs::write(
            &log,
            "INF Registered tunnel connection connIndex=0 protocol=quic\n",
        )
        .expect("write");
        assert!(log_shows_registered_connection(&log));
    }

    #[test]
    fn registration_must_postdate_last_unregistration() {
        let dir = tempdir().expect("tempdir");
        let log = dir.path().join("cloudflared.log");
        std::fs::write(
            &log,
            "INF Registered tunnel connection connIndex=0\n\
             INF Unregistered tunnel connection connIndex=0\n",
        )
        .expect("write");
        assert!(
            !log_shows_registered_connection(&log),
            "edge connection lost after registering → false"
        );
        std::fs::write(
            &log,
            "INF Registered tunnel connection connIndex=0\n\
             INF Unregistered tunnel connection connIndex=0\n\
             INF Registered tunnel connection connIndex=1\n",
        )
        .expect("write");
        assert!(
            log_shows_registered_connection(&log),
            "re-registered after a drop → true"
        );
        std::fs::write(&log, "INF Unregistered tunnel connection connIndex=0\n").expect("write");
        assert!(
            !log_shows_registered_connection(&log),
            "unregistration alone must not match the registered needle"
        );
    }

    #[test]
    fn local_evidence_reuses_fresh_and_lets_go_of_expired() {
        let url = "https://demo.trycloudflare.com";
        let expired = Some(DNS_WARMUP_DEADLINE + Duration::from_secs(1));
        // Fresh tunnel, alive and registered: reuse while DNS propagates.
        assert_eq!(
            classify_local_evidence(url, true, true, Some(Duration::from_secs(30)), true),
            RecordedTunnelState::WarmingUp
        );
        // Unknown age is no proof of death: keep the reuse bias.
        assert_eq!(
            classify_local_evidence(url, true, true, None, true),
            RecordedTunnelState::WarmingUp
        );
        // Past the warm-up deadline with the hostname confirmed absent from
        // public DNS: the tunnel is dead — let it go.
        assert_eq!(
            classify_local_evidence(url, true, true, expired, true),
            RecordedTunnelState::Down
        );
        // Past the deadline but no resolver answered: absence was never
        // confirmed, so there is no proof of death — keep reusing.
        assert_eq!(
            classify_local_evidence(url, true, true, expired, false),
            RecordedTunnelState::WarmingUp
        );
        // Dead process or lost edge registration: down regardless of age.
        assert_eq!(
            classify_local_evidence(url, false, true, Some(Duration::from_secs(30)), false),
            RecordedTunnelState::Down
        );
        assert_eq!(
            classify_local_evidence(url, true, false, Some(Duration::from_secs(30)), false),
            RecordedTunnelState::Down
        );
    }

    #[test]
    fn record_age_reads_url_file_mtime() {
        let dir = tempdir().expect("tempdir");
        let paths = shared_tunnel_paths_at(dir.path(), 8080);
        assert_eq!(record_age(&paths), None, "no record → no age");

        write_record(&paths, 4242, "https://demo.trycloudflare.com").expect("write record");
        let age = record_age(&paths).expect("age");
        assert!(age < Duration::from_secs(60), "fresh record: {age:?}");

        let spawned =
            std::time::SystemTime::now() - (DNS_WARMUP_DEADLINE + Duration::from_secs(60));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&paths.url_path)
            .expect("open url file");
        file.set_modified(spawned).expect("age url file");
        drop(file);
        let age = record_age(&paths).expect("age");
        assert!(age > DNS_WARMUP_DEADLINE, "aged record: {age:?}");
    }

    #[cfg(unix)]
    #[test]
    fn recorded_pid_identity_guards_against_reuse() {
        // This test process is not cloudflared, so its pid must fail the
        // identity check even though it is alive.
        assert!(process_alive(std::process::id()));
        assert!(!process_is_cloudflared(std::process::id()));
        // terminate_recorded_pid must refuse to kill it (we're still here to
        // assert afterwards precisely because it refused).
        terminate_recorded_pid(std::process::id());
        assert!(process_alive(std::process::id()));
    }

    #[test]
    fn url_host_extracts_hostname() {
        assert_eq!(
            url_host("https://foo-bar.trycloudflare.com/x").as_deref(),
            Some("foo-bar.trycloudflare.com")
        );
        assert_eq!(url_host("not a url"), None);
    }

    #[test]
    fn lock_acquire_release_and_stale_reclaim() {
        let dir = tempdir().expect("tempdir");
        let lock_path = dir.path().join("cloudflared-8080.lock");

        let lock = TunnelLock::acquire(&lock_path, Duration::from_millis(50)).expect("acquire");
        assert!(lock_path.exists());
        TunnelLock::acquire(&lock_path, Duration::from_millis(120))
            .expect_err("second acquire must time out while held");
        drop(lock);
        assert!(!lock_path.exists(), "drop must release the lock");

        std::fs::write(&lock_path, "12345").expect("plant lock");
        let stale = std::time::SystemTime::now() - (LOCK_STALE_AFTER + Duration::from_secs(60));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&lock_path)
            .expect("open lock");
        file.set_modified(stale).expect("age lock");
        drop(file);
        let _lock = TunnelLock::acquire(&lock_path, Duration::from_millis(50))
            .expect("stale lock must be reclaimed");
    }
}
