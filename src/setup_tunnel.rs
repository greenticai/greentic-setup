use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::{Map as JsonMap, Value};

pub struct SetupTunnel {
    pub mode: String,
    pub public_base_url: String,
    child: Child,
}

impl Drop for SetupTunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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
                    .is_some_and(|value| value.starts_with("https://"))
        })
}

pub fn start_setup_tunnel(mode: &str, local_base_url: &str) -> Result<SetupTunnel> {
    let mut command = match mode {
        "cloudflared" => {
            let mut command = Command::new("cloudflared");
            command.args(["tunnel", "--url", local_base_url, "--no-autoupdate"]);
            command
        }
        "ngrok" => {
            let mut command = Command::new("ngrok");
            command.args(["http", local_base_url, "--log=stdout"]);
            command
        }
        other => return Err(anyhow!("unsupported setup tunnel mode: {other}")),
    };
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .with_context(|| format!("start {mode} for setup OAuth callbacks"))?;

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
    for provider_answers in answers.values_mut() {
        let Some(obj) = provider_answers.as_object_mut() else {
            continue;
        };
        if !crate::provider_state::provider_enabled_from_map(obj) {
            continue;
        }
        if obj
            .get("public_base_url")
            .and_then(Value::as_str)
            .map(str::trim)
            .is_some_and(|value| value.starts_with("https://"))
        {
            continue;
        }
        obj.insert(
            "public_base_url".to_string(),
            Value::String(public_base_url.to_string()),
        );
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map as JsonMap, Value, json};

    use super::{
        extract_tunnel_https_url, inject_setup_public_base_url, should_start_setup_tunnel,
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
    }
}
