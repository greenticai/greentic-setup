//! Where a provider's `setup-answers.json` lives.
//!
//! Answers used to be keyed by provider alone
//! (`state/config/<provider>/setup-answers.json`) while every other piece of
//! per-provider setup state is tenant-scoped — so a second tenant read back the
//! first one's answers. Now: `state/config/<provider>/<tenant>/<team>/`.
//!
//! The tenant dir nests inside the provider dir so it cannot collide with the
//! well-known siblings under `state/config/` (`platform/`, `setup-actions/`).
//!
//! Compatibility: reads fall back to the legacy unscoped path; writes never
//! touch it. Older bundles keep working and a downgrade still reads its file.

use std::path::{Path, PathBuf};

const ANSWERS_FILE: &str = "setup-answers.json";

fn provider_dir(bundle_root: &Path, provider_id: &str) -> PathBuf {
    bundle_root.join("state").join("config").join(provider_id)
}

/// Where writes go.
pub fn answers_path(bundle_root: &Path, provider_id: &str, tenant: &str, team: &str) -> PathBuf {
    provider_dir(bundle_root, provider_id)
        .join(normalize_segment(tenant))
        .join(normalize_segment(team))
        .join(ANSWERS_FILE)
}

/// Pre-tenant path: read as a fallback, never written.
pub fn legacy_answers_path(bundle_root: &Path, provider_id: &str) -> PathBuf {
    provider_dir(bundle_root, provider_id).join(ANSWERS_FILE)
}

/// Scoped file if present, else legacy. Neither present → the scoped path, so
/// "missing" names what a write would create.
pub fn answers_path_for_read(
    bundle_root: &Path,
    provider_id: &str,
    tenant: &str,
    team: &str,
) -> PathBuf {
    let scoped = answers_path(bundle_root, provider_id, tenant, team);
    if scoped.is_file() {
        return scoped;
    }
    let legacy = legacy_answers_path(bundle_root, provider_id);
    if legacy.is_file() {
        return legacy;
    }
    scoped
}

/// Legacy first, then each `<tenant>/<team>`. For scans with no tenant in hand.
pub fn all_answers_paths(bundle_root: &Path, provider_id: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let legacy = legacy_answers_path(bundle_root, provider_id);
    if legacy.is_file() {
        found.push(legacy);
    }
    let dir = provider_dir(bundle_root, provider_id);
    let Ok(tenants) = std::fs::read_dir(&dir) else {
        return found;
    };
    let mut scoped = Vec::new();
    for tenant in tenants.flatten().filter(|e| e.path().is_dir()) {
        let Ok(teams) = std::fs::read_dir(tenant.path()) else {
            continue;
        };
        for team in teams.flatten().filter(|e| e.path().is_dir()) {
            let candidate = team.path().join(ANSWERS_FILE);
            if candidate.is_file() {
                scoped.push(candidate);
            }
        }
    }
    // Filesystem order is unstable; callers merge these last-write-wins.
    scoped.sort();
    found.extend(scoped);
    found
}

/// Keeps a tenant/team from escaping the provider dir. Empty → `default`.
fn normalize_segment(value: &str) -> String {
    let cleaned: String = value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "default".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_path_is_scoped_by_tenant_and_team() {
        let root = Path::new("/bundle");
        assert_eq!(
            answers_path(root, "messaging-slack", "koncar", "default"),
            Path::new("/bundle/state/config/messaging-slack/koncar/default/setup-answers.json")
        );
    }

    #[test]
    fn path_traversal_in_tenant_cannot_escape_the_provider_directory() {
        let root = Path::new("/bundle");
        let path = answers_path(root, "messaging-slack", "../../etc", "default");
        assert!(
            path.starts_with("/bundle/state/config/messaging-slack"),
            "escaped the provider dir: {}",
            path.display()
        );
    }

    #[test]
    fn empty_tenant_and_team_fall_back_to_default() {
        let root = Path::new("/bundle");
        assert_eq!(
            answers_path(root, "messaging-slack", "", ""),
            Path::new("/bundle/state/config/messaging-slack/default/default/setup-answers.json")
        );
    }

    #[test]
    fn read_prefers_the_scoped_file_but_falls_back_to_legacy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let legacy = legacy_answers_path(root, "messaging-slack");
        std::fs::create_dir_all(legacy.parent().unwrap()).expect("dir");
        std::fs::write(&legacy, "{}").expect("write legacy");

        // Only the legacy file exists: an older bundle keeps working.
        assert_eq!(
            answers_path_for_read(root, "messaging-slack", "koncar", "default"),
            legacy
        );

        // Once a tenant-scoped file exists it wins for that tenant...
        let scoped = answers_path(root, "messaging-slack", "koncar", "default");
        std::fs::create_dir_all(scoped.parent().unwrap()).expect("dir");
        std::fs::write(&scoped, "{}").expect("write scoped");
        assert_eq!(
            answers_path_for_read(root, "messaging-slack", "koncar", "default"),
            scoped
        );
        // ...and ONLY for that tenant: another tenant must not see it.
        assert_eq!(
            answers_path_for_read(root, "messaging-slack", "acme", "default"),
            legacy
        );
    }

    #[test]
    fn all_answers_paths_lists_legacy_then_every_scoped_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let legacy = legacy_answers_path(root, "messaging-slack");
        std::fs::create_dir_all(legacy.parent().unwrap()).expect("dir");
        std::fs::write(&legacy, "{}").expect("write legacy");
        for tenant in ["koncar", "acme"] {
            let scoped = answers_path(root, "messaging-slack", tenant, "default");
            std::fs::create_dir_all(scoped.parent().unwrap()).expect("dir");
            std::fs::write(&scoped, "{}").expect("write scoped");
        }

        let paths = all_answers_paths(root, "messaging-slack");

        assert_eq!(paths.len(), 3, "got {paths:?}");
        assert_eq!(paths[0], legacy, "legacy comes first");
        assert_eq!(
            paths[1],
            answers_path(root, "messaging-slack", "acme", "default")
        );
        assert_eq!(
            paths[2],
            answers_path(root, "messaging-slack", "koncar", "default")
        );
    }

    #[test]
    fn all_answers_paths_is_empty_when_the_provider_has_none() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert!(all_answers_paths(temp.path(), "messaging-slack").is_empty());
    }
}
