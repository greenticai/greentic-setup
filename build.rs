//! Embed the git short SHA (plus a `-dirty` marker) so every log line can
//! name the exact code that produced it — debugging a wizard session must
//! never again start with "which binary was this?".

use std::process::Command;

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .is_some_and(|out| !out.stdout.is_empty());
    let stamp = if dirty { format!("{sha}-dirty") } else { sha };
    println!("cargo:rustc-env=GREENTIC_SETUP_BUILD_SHA={stamp}");
    // Re-stamp when HEAD moves (best effort; a stale stamp only ever says
    // "-dirty" or an older sha, never silently lies about a clean build).
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
}
