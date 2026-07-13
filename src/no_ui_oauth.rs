use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use anyhow::{Context, Result, anyhow, bail};

use crate::setup_actions::{SetupAction, SetupActionKind, SetupActionStatus};

pub struct NoUiOAuthCallbackServer {
    pub local_base_url: String,
    result_rx: mpsc::Receiver<Result<String>>,
}

impl NoUiOAuthCallbackServer {
    pub fn wait_for_callback(self) -> Result<String> {
        self.result_rx
            .recv()
            .map_err(|_| anyhow!("OAuth callback server stopped before receiving a callback"))?
    }
}

pub fn start_callback_server(bundle_root: &Path, env: &str) -> Result<NoUiOAuthCallbackServer> {
    let listener = TcpListener::bind("127.0.0.1:0").context("bind no-UI OAuth callback server")?;
    let local_base_url = format!("http://{}", listener.local_addr()?);
    let bundle_root = bundle_root.to_path_buf();
    let env = env.to_string();
    let (result_tx, result_rx) = mpsc::channel();

    thread::spawn(move || {
        let result = run_callback_server(listener, bundle_root, env);
        let _ = result_tx.send(result);
    });

    Ok(NoUiOAuthCallbackServer {
        local_base_url,
        result_rx,
    })
}

pub fn pending_oauth_install_actions(actions: &[SetupAction]) -> Vec<&SetupAction> {
    actions
        .iter()
        .filter(|action| {
            action.kind == SetupActionKind::OauthInstallButton
                && action.status == SetupActionStatus::Pending
                && action.authorize_url.is_some()
        })
        .collect()
}

fn run_callback_server(listener: TcpListener, bundle_root: PathBuf, env: String) -> Result<String> {
    for stream in listener.incoming() {
        let mut stream = stream.context("accept OAuth callback")?;
        let request = match read_http_request(&mut stream) {
            Ok(request) => request,
            Err(err) => {
                let _ = write_response(
                    &mut stream,
                    400,
                    "OAuth setup failed",
                    &format!("Failed to read OAuth callback: {err}"),
                    false,
                );
                continue;
            }
        };
        let first_line = request.lines().next().unwrap_or("<empty>");
        eprintln!("[no-ui-oauth] request: {first_line}");
        let Some((path, query)) = parse_request_target(&request) else {
            let _ = write_response(
                &mut stream,
                400,
                "OAuth setup failed",
                "OAuth callback request was invalid.",
                false,
            );
            continue;
        };
        eprintln!(
            "[no-ui-oauth] parsed callback target: path={path} query_present={}",
            !query.is_empty()
        );
        if !path.starts_with("/oauth/callback/") {
            let _ = write_plain_response(&mut stream, 404, "Not found");
            continue;
        }

        let params = parse_query_params(&query);
        let code = params
            .iter()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.clone())
            .unwrap_or_default();
        let state = params
            .iter()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.clone())
            .unwrap_or_default();

        if code.is_empty() || state.is_empty() {
            eprintln!(
                "[no-ui-oauth] callback missing required query values: code_present={} state_present={}",
                !code.is_empty(),
                !state.is_empty()
            );
            let _ = write_response(
                &mut stream,
                400,
                "OAuth setup waiting",
                "OAuth callback missing code or state. Setup is still waiting for the provider redirect.",
                false,
            );
            continue;
        }

        eprintln!("[no-ui-oauth] completing OAuth callback");
        let runtime = tokio::runtime::Runtime::new().context("create OAuth callback runtime")?;
        let setup_callback_base = crate::oauth_callback::env_setup_callback_base();
        let result = runtime.block_on(crate::oauth_callback::complete_oauth_callback(
            &bundle_root,
            &env,
            &crate::oauth_callback::OAuthCallbackInput { code, state },
            "messaging.oauth.v1",
            setup_callback_base.as_deref(),
        ));

        return match result {
            Ok(report) => {
                let message = format!(
                    "OAuth setup complete for {} ({}/{})",
                    report.provider_id, report.tenant, report.team
                );
                write_response(
                    &mut stream,
                    200,
                    "OAuth setup complete",
                    &format!("{message}. You can close this tab and return to setup."),
                    true,
                )?;
                Ok(message)
            }
            Err(err) => {
                let message = format!("OAuth setup failed: {err:#}");
                write_response(&mut stream, 400, "OAuth setup failed", &message, false)?;
                Err(anyhow!(message))
            }
        };
    }

    bail!("OAuth callback server stopped before receiving a callback")
}

fn read_http_request(stream: &mut TcpStream) -> Result<String> {
    let mut buffer = [0_u8; 8192];
    let mut request = Vec::new();
    loop {
        let read = stream.read(&mut buffer).context("read HTTP request")?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") || request.len() >= 8192 {
            break;
        }
    }
    String::from_utf8(request).context("OAuth callback request was not UTF-8")
}

fn parse_request_target(request: &str) -> Option<(String, String)> {
    let first_line = request.lines().next()?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    if method != "GET" && method != "HEAD" {
        return None;
    }
    if target.starts_with("http://") || target.starts_with("https://") {
        let url = url::Url::parse(target).ok()?;
        return Some((
            url.path().to_string(),
            url.query().unwrap_or("").to_string(),
        ));
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    Some((path.to_string(), query.to_string()))
}

fn parse_query_params(query: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(query.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn write_plain_response(stream: &mut TcpStream, status: u16, body: &str) -> Result<()> {
    let reason = if status == 404 { "Not Found" } else { "OK" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .context("write response")
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    title: &str,
    message: &str,
    success: bool,
) -> Result<()> {
    let reason = if status < 400 { "OK" } else { "Bad Request" };
    let body = callback_page(success, title, message);
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .context("write response")
}

fn callback_page(success: bool, title: &str, message: &str) -> String {
    let status_class = if success { "success" } else { "error" };
    let close_script = if success {
        r#"<script>
setTimeout(function () {
  window.close();
}, 800);
</script>"#
    } else {
        ""
    };
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{title}</title>
  <style>
    body {{ margin: 0; min-height: 100vh; display: grid; place-items: center; font-family: system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; background: #f6f8fb; color: #17202a; }}
    main {{ width: min(520px, calc(100vw - 32px)); padding: 28px; border: 1px solid #d7dee8; border-radius: 8px; background: #fff; box-shadow: 0 16px 40px rgba(15, 23, 42, .08); }}
    h1 {{ margin: 0 0 12px; font-size: 1.35rem; line-height: 1.25; }}
    p {{ margin: 0; line-height: 1.55; color: #465466; }}
    .success h1 {{ color: #087f5b; }}
    .error h1 {{ color: #b42318; }}
  </style>
</head>
<body>
  <main class="{status_class}">
    <h1>{title}</h1>
    <p>{message}</p>
  </main>
  {close_script}
</body>
</html>"#,
        title = html_escape(title),
        message = html_escape(message),
        status_class = status_class,
        close_script = close_script
    )
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    use serde_json::Map as JsonMap;

    use crate::setup_actions::{SetupAction, SetupActionKind, SetupActionStatus};

    use super::{parse_query_params, parse_request_target, pending_oauth_install_actions};

    #[test]
    fn parses_oauth_callback_request_target() {
        let request =
            "GET /oauth/callback/slack?code=c&state=s HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let (path, query) = parse_request_target(request).expect("target");
        assert_eq!(path, "/oauth/callback/slack");
        assert_eq!(
            parse_query_params(&query),
            vec![
                ("code".to_string(), "c".to_string()),
                ("state".to_string(), "s".to_string())
            ]
        );
    }

    #[test]
    fn parses_absolute_form_oauth_callback_request_target() {
        let request = "GET https://modern-retreat-checking-longitude.trycloudflare.com/oauth/callback/slack?code=c&state=s HTTP/1.1\r\nHost: modern-retreat-checking-longitude.trycloudflare.com\r\n\r\n";
        let (path, query) = parse_request_target(request).expect("target");
        assert_eq!(path, "/oauth/callback/slack");
        assert_eq!(
            parse_query_params(&query),
            vec![
                ("code".to_string(), "c".to_string()),
                ("state".to_string(), "s".to_string())
            ]
        );
    }

    #[test]
    fn parses_head_oauth_callback_request_target() {
        let request =
            "HEAD /oauth/callback/slack?code=c&state=s HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let (path, query) = parse_request_target(request).expect("target");
        assert_eq!(path, "/oauth/callback/slack");
        assert_eq!(
            parse_query_params(&query),
            vec![
                ("code".to_string(), "c".to_string()),
                ("state".to_string(), "s".to_string())
            ]
        );
    }

    #[test]
    fn waits_for_pending_oauth_install_even_without_action_callback_path() {
        let action = SetupAction {
            id: "add_to_slack".to_string(),
            kind: SetupActionKind::OauthInstallButton,
            label: "Add to Slack".to_string(),
            provider_id: "messaging-slack".to_string(),
            tenant: "default".to_string(),
            team: Some("default".to_string()),
            authorize_url: Some("https://slack.com/oauth/v2/authorize".to_string()),
            callback_path: None,
            state: Some("state".to_string()),
            status: SetupActionStatus::Pending,
            created_at: None,
            completed_at: None,
            extra: JsonMap::new(),
        };

        assert_eq!(pending_oauth_install_actions(&[action]).len(), 1);
    }

    #[test]
    fn callback_server_keeps_waiting_after_probe_without_code_or_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let server = super::start_callback_server(temp.path(), "dev").expect("server");
        let addr = server
            .local_base_url
            .strip_prefix("http://")
            .expect("local url")
            .to_string();

        let first = send_get(&addr, "/oauth/callback/slack");
        assert!(first.contains("400 Bad Request"));
        assert!(first.contains("OAuth setup waiting"));

        let second = send_get(&addr, "/not-found");
        assert!(second.contains("404 Not Found"));
    }

    fn send_get(addr: &str, path: &str) -> String {
        let mut stream = TcpStream::connect(addr).expect("connect");
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).expect("write");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read");
        response
    }
}
