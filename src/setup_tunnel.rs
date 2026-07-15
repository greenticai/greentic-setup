use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::{Map as JsonMap, Value};

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

pub fn should_start_setup_tunnel(mode: &str, answers: &JsonMap<String, Value>) -> bool {
    matches!(mode, "cloudflared" | "ngrok")
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

pub fn start_setup_tunnel(mode: &str, local_base_url: &str) -> Result<SetupTunnel> {
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
        other => Err(anyhow!("unsupported setup tunnel mode: {other}")),
    }
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

pub fn is_ephemeral_tunnel_url(value: &str) -> bool {
    url::Url::parse(value).ok().is_some_and(|url| {
        url.scheme() == "https"
            && url.host_str().is_some_and(|host| {
                let host = host.to_ascii_lowercase();
                host == "trycloudflare.com"
                    || host.ends_with(".trycloudflare.com")
                    || host.ends_with(".ngrok-free.app")
                    || host.ends_with(".ngrok.io")
            })
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::{Map as JsonMap, Value, json};

    use super::*;

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
        let result = start_setup_tunnel("unknown", "http://127.0.0.1:8080");
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
