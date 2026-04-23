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
fn top_level_help_offers_safe_passphrase_sources_only() {
    let help = run_help(&["--help"]);
    assert_no_passphrase_value_flag(&help, "top-level");
    assert!(
        help.contains("--passphrase-stdin"),
        "expected --passphrase-stdin in top-level help (it's a global flag)"
    );
    assert!(
        help.contains("--passphrase-file"),
        "expected --passphrase-file in top-level help (it's a global flag)"
    );
}

#[test]
fn bundle_setup_help_never_lists_inline_passphrase_value_flag() {
    // Global passphrase flags are intentionally NOT listed in
    // subcommand help (they're only at top level). What we MUST
    // guarantee is that no --passphrase=<value> flag was
    // accidentally added at the subcommand level either.
    let help = run_help(&["bundle", "setup", "--help"]);
    assert_no_passphrase_value_flag(&help, "bundle setup");
}

#[test]
fn bundle_update_help_never_lists_inline_passphrase_value_flag() {
    let help = run_help(&["bundle", "update", "--help"]);
    assert_no_passphrase_value_flag(&help, "bundle update");
}
