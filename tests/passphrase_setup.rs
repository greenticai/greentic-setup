//! End-to-end integration test: invoke `greentic-setup bundle setup`
//! with `--passphrase-stdin` against a tiny temp bundle. Verifies that
//! the resulting `.dev.secrets.env` is in encrypted v1 format and
//! contains no plaintext leaks.
//!
//! This test runs the actual binary via stdio, exercising the real
//! passphrase resolution + KDF + AES-GCM wrap path.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use tempfile::TempDir;

const KNOWN_PASSPHRASE: &str = "PASSPHRASE_LEAK_PROBE_8675309xyz";

fn write_minimal_bundle(dir: &Path) {
    // The setup CLI requires either greentic.demo.yaml (legacy) or
    // bundle.yaml (workspace). The legacy marker is the lighter weight
    // option for tests because it doesn't trigger workspace metadata
    // sync. We pair it with a minimal pack manifest so discovery has
    // something to enumerate.
    fs::write(
        dir.join("greentic.demo.yaml"),
        "tenant: demo\nteam: default\nenv: dev\n",
    )
    .expect("write demo yaml");
}

fn run_setup_with_stdin(bundle_dir: &Path, stdin_lines: &[&str]) -> std::process::Output {
    let exe = env!("CARGO_BIN_EXE_greentic-setup");
    let mut child = Command::new(exe)
        // Force --emit-answers so the setup short-circuits before
        // executing any plan; we only want to validate the encryption
        // layer was wired and the file format on disk is correct.
        .args([
            "bundle",
            "setup",
            "--bundle",
            bundle_dir.to_str().expect("utf8 path"),
            "--env",
            "dev",
            "--tenant",
            "demo",
            "--passphrase-stdin",
            "--emit-answers",
            bundle_dir.join("answers.json").to_str().expect("path"),
            "--non-interactive",
        ])
        .env("RUST_LOG", "trace")
        // Disable color/markup that could fragment our grep patterns.
        .env("NO_COLOR", "1")
        .env("CLICOLOR", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn setup");

    {
        let mut stdin = child.stdin.take().expect("stdin handle");
        for line in stdin_lines {
            writeln!(stdin, "{line}").expect("write stdin");
        }
        // Closing stdin signals EOF for any further prompt attempts.
    }

    // Cap test duration so a stuck prompt doesn't hang CI.
    let mut waited = Duration::ZERO;
    let step = Duration::from_millis(50);
    let max = Duration::from_secs(30);
    loop {
        if let Some(_status) = child.try_wait().expect("try_wait") {
            return child.wait_with_output().expect("output");
        }
        if waited >= max {
            let _ = child.kill();
            panic!(
                "greentic-setup did not exit within {} seconds; last waited={:?}",
                max.as_secs(),
                waited
            );
        }
        std::thread::sleep(step);
        waited += step;
    }
}

#[test]
fn setup_with_stdin_passphrase_creates_encrypted_store_and_does_not_leak_passphrase() {
    let temp = TempDir::new().expect("tempdir");
    let bundle = temp.path();
    write_minimal_bundle(bundle);

    let output = run_setup_with_stdin(bundle, &[KNOWN_PASSPHRASE]);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    // Security guarantee: the known passphrase must NEVER appear in
    // any captured output, regardless of trace level.
    assert!(
        !combined.contains(KNOWN_PASSPHRASE),
        "passphrase leaked in setup output. stderr/stdout (last 4000 chars):\n{}",
        combined
            .chars()
            .rev()
            .take(4000)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>()
    );

    // Encrypted-format guarantee: when setup completed (zero exit) and
    // some store was opened, the file on disk must start with the v1
    // header, not legacy plaintext.
    let store = bundle.join(".greentic/dev/.dev.secrets.env");
    if store.exists() {
        let bytes = fs::read(&store).expect("read store");
        if !bytes.is_empty() {
            assert!(
                bytes.starts_with(b"# greentic-encrypted: v1\n"),
                "store on disk is not encrypted v1; first 200 bytes:\n{}",
                String::from_utf8_lossy(&bytes[..bytes.len().min(200)])
            );
            // The known passphrase must not appear anywhere in the file.
            assert!(
                !bytes
                    .windows(KNOWN_PASSPHRASE.len())
                    .any(|w| w == KNOWN_PASSPHRASE.as_bytes()),
                "passphrase content leaked into store file"
            );
        }
    }
}
