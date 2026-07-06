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
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A lock file untouched for this long belongs to a crashed process and may
/// be reclaimed. Spawn + URL discovery hold the lock for well under a minute.
const LOCK_STALE_AFTER: Duration = Duration::from_secs(120);

/// Probe budget when deciding whether a recorded tunnel still serves.
const REUSE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

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

fn shared_tunnel_paths_at(root: &Path, port: u16) -> SharedTunnelPaths {
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

/// Terminate the recorded process. Only ever called with a PID read from the
/// shared record — ownership is proven by the record, never by process name.
pub fn terminate_recorded_pid(pid: u32) {
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

/// One HEAD probe. `true` = the edge routed to a live origin (2xx/3xx, or any
/// error status other than Cloudflare's 530 tunnel-down page — a 404 from the
/// origin still proves the tunnel works). Transport errors and 530 = dead.
fn head_probe(url: &str) -> bool {
    match ureq::head(url).call() {
        Ok(_) => true,
        Err(ureq::Error::StatusCode(code)) => code != 530,
        Err(_) => false,
    }
}

/// Probe with retries until `REUSE_PROBE_TIMEOUT` elapses.
pub fn probe_tunnel_alive(url: &str) -> bool {
    let deadline = Instant::now() + REUSE_PROBE_TIMEOUT;
    loop {
        if head_probe(url) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(300));
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
