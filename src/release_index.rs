//! Resolve a tag-pinned pack reference to the digest the INSTALLED toolchain
//! release pinned it to, using the release index `gtc` already wrote to disk.
//!
//! Why this exists: the provider catalogue pins packs by TAG
//! (`oci://…/messaging-slack:stable`), and `greentic-distributor-client`'s
//! `fetch_pack_to_cache` only consults its local cache when the reference is
//! DIGEST-pinned — a tagged reference skips the cache lookup entirely and always
//! pulls. The cache is therefore write-only for tags: it accumulates artifacts
//! (82 packs on a typical box) and never serves one. Adding a provider hits the
//! network every time, and fails outright when the registry is unreachable.
//!
//! Nothing new has to be cached to fix that. When `gtc` installs a toolchain it
//! writes `<cache>/release-index/v1/<channel>/<release>.json`
//! (`greentic.release-index.v1`), mapping every tagged reference in the release
//! to `{version, digest, canonical_ref}`. Resolving through it turns a tag into
//! the digest-pinned reference the fetcher CAN serve from cache — no change to
//! cache semantics, no staleness trade-off, and adds become content-pinned
//! rather than trusting a mutable tag.
//!
//! Deliberately keyed to the INSTALLED release (`<greentic-home>/toolchain/
//! installed.json`, schema `greentic.installed-toolchain.v1`) rather than the
//! newest index on disk. A box can hold indexes for several releases, and
//! `:stable` resolves to different versions in each — on the box this was
//! written against, release 1.1.10 pinned `messaging-slack` to 0.5.18 while the
//! newer 1.1.13 pinned 0.5.21. Picking "newest index" would silently install a
//! pack the installed toolchain never intended. Note also that
//! `<greentic-home>/releases/current.json` is NOT the right pointer — it can lag
//! well behind (it read 1.0.4 while the installed toolchain was 1.1.10).
//!
//! Every step is best-effort: no toolchain record, no index for that release, or
//! no entry for the reference all fall back to the original tagged reference and
//! the previous network behaviour.

use std::path::{Path, PathBuf};

use serde::Deserialize;

const RELEASE_INDEX_SCHEMA: &str = "greentic.release-index.v1";
const INSTALLED_TOOLCHAIN_SCHEMA: &str = "greentic.installed-toolchain.v1";

/// `<greentic-home>` — `GREENTIC_HOME` when set, else `~/.greentic`.
fn greentic_home() -> Option<PathBuf> {
    if let Ok(root) = std::env::var("GREENTIC_HOME") {
        return Some(PathBuf::from(root));
    }
    std::env::var("HOME").ok().map(|home| {
        let mut path = PathBuf::from(home);
        path.push(".greentic");
        path
    })
}

/// Distribution cache root. Mirrors `greentic-distributor-client`'s resolution
/// order — `GREENTIC_CACHE_DIR`, then `GREENTIC_DIST_CACHE_DIR`, then
/// `<greentic-home>/cache/distribution` — because we must read the same
/// directory that crate writes. Its own resolver is private, so this has to stay
/// in sync by convention.
fn distribution_cache_root() -> PathBuf {
    if let Some(dir) = std::env::var("GREENTIC_CACHE_DIR")
        .or_else(|_| std::env::var("GREENTIC_DIST_CACHE_DIR"))
        .ok()
        .map(PathBuf::from)
    {
        return dir;
    }
    match greentic_home() {
        Some(home) => home.join("cache").join("distribution"),
        None => PathBuf::from(".greentic")
            .join("cache")
            .join("distribution"),
    }
}

#[derive(Debug, Deserialize)]
struct InstalledToolchain {
    schema: String,
    channel: Option<String>,
    version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReleaseIndex {
    schema: String,
    #[serde(default)]
    refs: std::collections::BTreeMap<String, ReleaseIndexEntry>,
}

#[derive(Debug, Deserialize)]
struct ReleaseIndexEntry {
    #[serde(default)]
    digest: Option<String>,
    #[serde(default)]
    canonical_ref: Option<String>,
}

/// The `(channel, release)` of the installed toolchain.
fn installed_release_in(toolchain_root: &Path) -> Option<(String, String)> {
    let raw = std::fs::read_to_string(toolchain_root.join("installed.json")).ok()?;
    let installed: InstalledToolchain = serde_json::from_str(&raw).ok()?;
    if installed.schema != INSTALLED_TOOLCHAIN_SCHEMA {
        return None;
    }
    let channel = installed.channel?.trim().to_string();
    let version = installed.version?.trim().to_string();
    (!channel.is_empty() && !version.is_empty()).then_some((channel, version))
}

/// Digest-pinned equivalent of `reference` per the release index, or `None` when
/// it cannot be resolved (caller keeps the tag and pulls as before).
///
/// `reference` may carry the `oci://` scheme; index keys do not.
pub(crate) fn pinned_ref_for(reference: &str) -> Option<String> {
    let home = greentic_home()?;
    pinned_ref_for_in(
        &distribution_cache_root(),
        &home.join("toolchain"),
        reference,
    )
}

/// [`pinned_ref_for`] with both roots supplied, so tests need no process-env
/// mutation (this crate has no `temp-env` dev-dependency).
pub(crate) fn pinned_ref_for_in(
    cache_root: &Path,
    toolchain_root: &Path,
    reference: &str,
) -> Option<String> {
    // Already digest-pinned: the fetcher can serve it from cache as-is.
    if reference.contains("@sha256:") {
        return None;
    }
    let (channel, release) = installed_release_in(toolchain_root)?;
    let index_path = cache_root
        .join("release-index")
        .join("v1")
        .join(&channel)
        .join(format!("{release}.json"));
    let raw = std::fs::read_to_string(&index_path).ok()?;
    let index: ReleaseIndex = serde_json::from_str(&raw).ok()?;
    if index.schema != RELEASE_INDEX_SCHEMA {
        return None;
    }

    let key = reference.strip_prefix("oci://").unwrap_or(reference).trim();
    let entry = index.refs.get(key)?;

    // Prefer the index's own canonical form; fall back to composing one from the
    // digest so a partially-populated entry still resolves.
    if let Some(canonical) = entry
        .canonical_ref
        .as_deref()
        .map(str::trim)
        .filter(|value| value.contains("@sha256:"))
    {
        return Some(canonical.to_string());
    }
    let digest = entry
        .digest
        .as_deref()
        .map(str::trim)
        .filter(|digest| digest.starts_with("sha256:"))?;
    let repo = key.rsplit_once(':').map(|(repo, _tag)| repo).unwrap_or(key);
    Some(format!("oci://{repo}@{digest}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLACK_TAG: &str = "oci://ghcr.io/greenticai/packs/messaging/messaging-slack:stable";
    const SLACK_DIGEST: &str =
        "sha256:163a5c0ab582798a0000000000000000000000000000000000000000000000ab";

    fn write(path: PathBuf, body: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, body).expect("write");
    }

    /// Stage an installed-toolchain record plus a matching release index.
    fn stage(dir: &Path, release: &str, index_body: &str) -> (PathBuf, PathBuf) {
        let cache_root = dir.join("cache").join("distribution");
        let toolchain_root = dir.join("toolchain");
        write(
            toolchain_root.join("installed.json"),
            &format!(
                r#"{{"schema":"greentic.installed-toolchain.v1","channel":"stable","version":"{release}"}}"#
            ),
        );
        write(
            cache_root
                .join("release-index")
                .join("v1")
                .join("stable")
                .join(format!("{release}.json")),
            index_body,
        );
        (cache_root, toolchain_root)
    }

    fn index_with_slack(canonical: bool) -> String {
        let entry = if canonical {
            format!(
                r#"{{"version":"0.5.18","digest":"{SLACK_DIGEST}","canonical_ref":"oci://ghcr.io/greenticai/packs/messaging/messaging-slack@{SLACK_DIGEST}"}}"#
            )
        } else {
            format!(r#"{{"version":"0.5.18","digest":"{SLACK_DIGEST}"}}"#)
        };
        format!(
            r#"{{"schema":"greentic.release-index.v1","refs":{{"ghcr.io/greenticai/packs/messaging/messaging-slack:stable":{entry}}}}}"#
        )
    }

    #[test]
    fn resolves_a_tag_to_the_indexed_canonical_ref() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (cache, toolchain) = stage(dir.path(), "1.1.10", &index_with_slack(true));
        assert_eq!(
            pinned_ref_for_in(&cache, &toolchain, SLACK_TAG).as_deref(),
            Some(
                format!("oci://ghcr.io/greenticai/packs/messaging/messaging-slack@{SLACK_DIGEST}")
                    .as_str()
            ),
            "a tagged ref must resolve to the digest the installed release pinned"
        );
    }

    #[test]
    fn composes_a_pinned_ref_when_the_entry_has_only_a_digest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (cache, toolchain) = stage(dir.path(), "1.1.10", &index_with_slack(false));
        let resolved = pinned_ref_for_in(&cache, &toolchain, SLACK_TAG).expect("resolves");
        assert!(
            resolved.ends_with(&format!("@{SLACK_DIGEST}")),
            "{resolved}"
        );
        assert!(!resolved.contains(":stable"), "tag must be dropped");
    }

    #[test]
    fn accepts_a_reference_without_the_oci_scheme() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (cache, toolchain) = stage(dir.path(), "1.1.10", &index_with_slack(true));
        assert!(
            pinned_ref_for_in(
                &cache,
                &toolchain,
                "ghcr.io/greenticai/packs/messaging/messaging-slack:stable"
            )
            .is_some(),
            "index keys carry no scheme, so both spellings must resolve"
        );
    }

    #[test]
    fn resolves_against_the_installed_release_not_the_newest_index() {
        // The regression this guards: a box holding several indexes must use the
        // one for the INSTALLED release. `:stable` means different versions in
        // different releases, so picking the newest silently installs a pack the
        // installed toolchain never pinned.
        let dir = tempfile::tempdir().expect("tempdir");
        let (cache, toolchain) = stage(dir.path(), "1.1.10", &index_with_slack(true));
        // A NEWER index exists but pins a different digest.
        write(
            cache
                .join("release-index")
                .join("v1")
                .join("stable")
                .join("1.1.13.json"),
            r#"{"schema":"greentic.release-index.v1","refs":{"ghcr.io/greenticai/packs/messaging/messaging-slack:stable":{"version":"0.5.21","digest":"sha256:deadbeef00000000000000000000000000000000000000000000000000000000"}}}"#,
        );
        let resolved = pinned_ref_for_in(&cache, &toolchain, SLACK_TAG).expect("resolves");
        assert!(
            resolved.contains(SLACK_DIGEST),
            "must use 1.1.10's digest, got {resolved}"
        );
        assert!(
            !resolved.contains("deadbeef"),
            "must not use 1.1.13's digest"
        );
    }

    #[test]
    fn falls_back_to_the_tag_when_resolution_is_not_possible() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (cache, toolchain) = stage(dir.path(), "1.1.10", &index_with_slack(true));

        // A reference the index does not carry.
        assert_eq!(
            pinned_ref_for_in(&cache, &toolchain, "oci://ghcr.io/other/pack:stable"),
            None
        );
        // Already digest-pinned — nothing to do.
        assert_eq!(
            pinned_ref_for_in(
                &cache,
                &toolchain,
                &format!("oci://ghcr.io/greenticai/packs/messaging/messaging-slack@{SLACK_DIGEST}")
            ),
            None
        );

        // No index for the installed release.
        let bare = tempfile::tempdir().expect("tempdir");
        let (bare_cache, bare_toolchain) = stage(bare.path(), "1.1.10", &index_with_slack(true));
        std::fs::remove_file(
            bare_cache
                .join("release-index")
                .join("v1")
                .join("stable")
                .join("1.1.10.json"),
        )
        .expect("remove index");
        assert_eq!(
            pinned_ref_for_in(&bare_cache, &bare_toolchain, SLACK_TAG),
            None
        );

        // No installed-toolchain record at all.
        let empty = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            pinned_ref_for_in(empty.path(), empty.path(), SLACK_TAG),
            None
        );
    }

    #[test]
    fn rejects_unexpected_schemas() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (cache, toolchain) = stage(dir.path(), "1.1.10", &index_with_slack(true));
        // Wrong index schema.
        write(
            cache
                .join("release-index")
                .join("v1")
                .join("stable")
                .join("1.1.10.json"),
            r#"{"schema":"something.else.v9","refs":{}}"#,
        );
        assert_eq!(pinned_ref_for_in(&cache, &toolchain, SLACK_TAG), None);

        // Wrong installed-toolchain schema.
        let other = tempfile::tempdir().expect("tempdir");
        let (other_cache, other_toolchain) = stage(other.path(), "1.1.10", &index_with_slack(true));
        write(
            other_toolchain.join("installed.json"),
            r#"{"schema":"nope.v1","channel":"stable","version":"1.1.10"}"#,
        );
        assert_eq!(
            pinned_ref_for_in(&other_cache, &other_toolchain, SLACK_TAG),
            None
        );
    }
}
