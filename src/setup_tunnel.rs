use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::{Map as JsonMap, Value};

pub struct SetupTunnel {
    pub mode: String,
    pub local_base_url: String,
    pub public_base_url: String,
    child: Child,
}

impl Drop for SetupTunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl SetupTunnel {
    pub fn is_running(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
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
                    return Ok(SetupTunnel {
                        mode: mode.to_string(),
                        local_base_url: local_base_url.trim_end_matches('/').to_string(),
                        public_base_url: url,
                        child,
                    });
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
    let mut response = ureq::get(url)
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
    use serde_json::{Map as JsonMap, Value, json};

    use super::{
        extract_tunnel_https_url, inject_setup_public_base_url, is_ephemeral_tunnel_url,
        should_start_setup_tunnel,
    };

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
    fn detects_ephemeral_tunnel_urls() {
        assert!(is_ephemeral_tunnel_url("https://demo.trycloudflare.com"));
        assert!(is_ephemeral_tunnel_url("https://demo.ngrok-free.app"));
        assert!(is_ephemeral_tunnel_url("https://demo.ngrok.io"));
        assert!(!is_ephemeral_tunnel_url("https://runtime.example.com"));
    }
}
