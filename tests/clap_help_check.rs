//! Compile-time security regression: ensure no `--passphrase=<value>`
//! flag is ever introduced on the setup CLI.
//!
//! Passphrases must come from TTY prompt, stdin pipe, or 0600-mode
//! file only — never from argv (visible via `ps -o args` or
//! `/proc/PID/cmdline`).

use std::process::Command;

fn run_help(args: &[&str]) -> String {
    let exe = env!("CARGO_BIN_EXE_greentic-setup");
    let out = Command::new(exe)
        .args(args)
        .output()
        .expect("invoking greentic-setup --help");
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&out.stdout));
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    combined
}

fn assert_no_passphrase_value_flag(help: &str, label: &str) {
    let forbidden_patterns = [
        "--passphrase ",  // clap renders required values as `--flag <VALUE>` with a space
        "--passphrase=",  // long-form value flag
        "--passphrase\n", // bare flag (would imply value-taking)
        "--passphrase\t",
    ];
    for pat in forbidden_patterns {
        assert!(
            !help.contains(pat),
            "[{label}] forbidden pattern `{pat:?}` found in help; passphrase must never be a CLI value.\nHelp output:\n{help}"
        );
    }
}

#[test]
fn bundle_setup_help_does_not_offer_inline_passphrase_flag() {
    let help = run_help(&["bundle", "setup", "--help"]);
    assert_no_passphrase_value_flag(&help, "bundle setup");
    assert!(
        help.contains("--passphrase-stdin"),
        "expected --passphrase-stdin in help"
    );
    assert!(
        help.contains("--passphrase-file"),
        "expected --passphrase-file in help"
    );
}

#[test]
fn bundle_update_help_does_not_offer_inline_passphrase_flag() {
    let help = run_help(&["bundle", "update", "--help"]);
    assert_no_passphrase_value_flag(&help, "bundle update");
    assert!(help.contains("--passphrase-stdin"));
    assert!(help.contains("--passphrase-file"));
}
