//! Hot reload types and diffing for bundle changes.
//!
//! When a bundle is updated via the admin API, the reload module computes
//! what changed (added/removed/changed packs, providers, tenants) and
//! produces a [`ReloadPlan`] that the consuming runtime can apply.
//!
//! The actual runtime reload (swapping `Arc<RunnerHost>`, draining connections)
//! lives in the consuming crate (e.g. greentic-operator).

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::discovery::{DetectedProvider, DiscoveryResult};

/// A computed diff between two bundle states.
#[derive(Clone, Debug, Default, Serialize)]
pub struct BundleDiff {
    /// Packs added since the last state.
    pub packs_added: Vec<DetectedProvider>,
    /// Packs removed since the last state.
    pub packs_removed: Vec<DetectedProvider>,
    /// Packs that changed (same provider_id, different file content).
    pub packs_changed: Vec<DetectedProvider>,
    /// Provider IDs added to the registry.
    pub providers_added: Vec<String>,
    /// Provider IDs removed from the registry.
    pub providers_removed: Vec<String>,
    /// Tenants added.
    pub tenants_added: Vec<String>,
    /// Tenants removed.
    pub tenants_removed: Vec<String>,
}

impl BundleDiff {
    /// Returns `true` if there are no changes.
    pub fn is_empty(&self) -> bool {
        self.packs_added.is_empty()
            && self.packs_removed.is_empty()
            && self.packs_changed.is_empty()
            && self.providers_added.is_empty()
            && self.providers_removed.is_empty()
            && self.tenants_added.is_empty()
            && self.tenants_removed.is_empty()
    }

    /// Total number of changes across all categories.
    pub fn change_count(&self) -> usize {
        self.packs_added.len()
            + self.packs_removed.len()
            + self.packs_changed.len()
            + self.providers_added.len()
            + self.providers_removed.len()
            + self.tenants_added.len()
            + self.tenants_removed.len()
    }
}

/// A plan for applying a bundle diff at runtime.
#[derive(Clone, Debug, Serialize)]
pub struct ReloadPlan {
    pub bundle: PathBuf,
    pub diff: BundleDiff,
    pub actions: Vec<ReloadAction>,
}

/// Individual reload action.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReloadAction {
    /// Load a new WASM component into the runtime.
    LoadComponent { provider_id: String, path: PathBuf },
    /// Unload a WASM component from the runtime.
    UnloadComponent { provider_id: String },
    /// Reload a changed WASM component (unload + load).
    ReloadComponent { provider_id: String, path: PathBuf },
    /// Update the provider route table.
    UpdateRoutes,
    /// Re-run the resolver to regenerate manifests.
    RunResolver,
    /// Seed secrets for newly added packs.
    SeedSecrets { provider_id: String },
}

/// Compute the diff between two discovery results (previous and current state).
pub fn diff_discoveries(prev: &DiscoveryResult, curr: &DiscoveryResult) -> BundleDiff {
    let prev_ids: BTreeSet<&str> = prev
        .providers
        .iter()
        .map(|p| p.provider_id.as_str())
        .collect();
    let curr_ids: BTreeSet<&str> = curr
        .providers
        .iter()
        .map(|p| p.provider_id.as_str())
        .collect();

    let added_ids: BTreeSet<&&str> = curr_ids.difference(&prev_ids).collect();
    let removed_ids: BTreeSet<&&str> = prev_ids.difference(&curr_ids).collect();

    let packs_added: Vec<DetectedProvider> = curr
        .providers
        .iter()
        .filter(|p| added_ids.contains(&&p.provider_id.as_str()))
        .cloned()
        .collect();

    let packs_removed: Vec<DetectedProvider> = prev
        .providers
        .iter()
        .filter(|p| removed_ids.contains(&&p.provider_id.as_str()))
        .cloned()
        .collect();

    // Changed packs: same ID but different file path (content change detection
    // via path — full content hashing is left to the consumer).
    let packs_changed: Vec<DetectedProvider> = curr
        .providers
        .iter()
        .filter(|cp| {
            if added_ids.contains(&&cp.provider_id.as_str()) {
                return false;
            }
            prev.providers
                .iter()
                .any(|pp| pp.provider_id == cp.provider_id && pp.pack_path != cp.pack_path)
        })
        .cloned()
        .collect();

    BundleDiff {
        packs_added,
        packs_removed,
        packs_changed,
        providers_added: Vec::new(),
        providers_removed: Vec::new(),
        tenants_added: Vec::new(),
        tenants_removed: Vec::new(),
    }
}

/// Build a reload plan from a bundle diff.
pub fn plan_reload(bundle: &std::path::Path, diff: &BundleDiff) -> ReloadPlan {
    let mut actions = Vec::new();

    for pack in &diff.packs_added {
        actions.push(ReloadAction::LoadComponent {
            provider_id: pack.provider_id.clone(),
            path: pack.pack_path.clone(),
        });
        actions.push(ReloadAction::SeedSecrets {
            provider_id: pack.provider_id.clone(),
        });
    }

    for pack in &diff.packs_removed {
        actions.push(ReloadAction::UnloadComponent {
            provider_id: pack.provider_id.clone(),
        });
    }

    for pack in &diff.packs_changed {
        actions.push(ReloadAction::ReloadComponent {
            provider_id: pack.provider_id.clone(),
            path: pack.pack_path.clone(),
        });
    }

    if !diff.packs_added.is_empty()
        || !diff.packs_removed.is_empty()
        || !diff.packs_changed.is_empty()
    {
        actions.push(ReloadAction::UpdateRoutes);
    }

    if !diff.tenants_added.is_empty() || !diff.tenants_removed.is_empty() {
        actions.push(ReloadAction::RunResolver);
    }

    ReloadPlan {
        bundle: bundle.to_path_buf(),
        diff: diff.clone(),
        actions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{DetectedDomains, ProviderIdSource};

    fn make_provider(id: &str, path: &str) -> DetectedProvider {
        DetectedProvider {
            provider_id: id.to_string(),
            domain: "messaging".to_string(),
            pack_path: PathBuf::from(path),
            id_source: ProviderIdSource::Manifest,
        }
    }

    fn make_discovery(providers: Vec<DetectedProvider>) -> DiscoveryResult {
        DiscoveryResult {
            domains: DetectedDomains {
                messaging: true,
                events: false,
                oauth: false,
                state: false,
                secrets: false,
            },
            providers,
        }
    }

    #[test]
    fn empty_diff_when_same() {
        let disc = make_discovery(vec![make_provider("telegram", "/a/telegram.gtpack")]);
        let diff = diff_discoveries(&disc, &disc);
        assert!(diff.is_empty());
        assert_eq!(diff.change_count(), 0);
    }

    #[test]
    fn detects_added_packs() {
        let prev = make_discovery(vec![make_provider("telegram", "/a/telegram.gtpack")]);
        let curr = make_discovery(vec![
            make_provider("telegram", "/a/telegram.gtpack"),
            make_provider("slack", "/a/slack.gtpack"),
        ]);
        let diff = diff_discoveries(&prev, &curr);
        assert_eq!(diff.packs_added.len(), 1);
        assert_eq!(diff.packs_added[0].provider_id, "slack");
        assert!(diff.packs_removed.is_empty());
    }

    #[test]
    fn detects_removed_packs() {
        let prev = make_discovery(vec![
            make_provider("telegram", "/a/telegram.gtpack"),
            make_provider("slack", "/a/slack.gtpack"),
        ]);
        let curr = make_discovery(vec![make_provider("telegram", "/a/telegram.gtpack")]);
        let diff = diff_discoveries(&prev, &curr);
        assert!(diff.packs_added.is_empty());
        assert_eq!(diff.packs_removed.len(), 1);
        assert_eq!(diff.packs_removed[0].provider_id, "slack");
    }

    #[test]
    fn detects_changed_packs() {
        let prev = make_discovery(vec![make_provider("telegram", "/a/v1/telegram.gtpack")]);
        let curr = make_discovery(vec![make_provider("telegram", "/a/v2/telegram.gtpack")]);
        let diff = diff_discoveries(&prev, &curr);
        assert!(diff.packs_added.is_empty());
        assert!(diff.packs_removed.is_empty());
        assert_eq!(diff.packs_changed.len(), 1);
    }

    #[test]
    fn plan_reload_generates_actions() {
        let diff = BundleDiff {
            packs_added: vec![make_provider("slack", "/a/slack.gtpack")],
            packs_removed: vec![make_provider("teams", "/a/teams.gtpack")],
            packs_changed: vec![make_provider("telegram", "/a/telegram.gtpack")],
            ..Default::default()
        };
        let plan = plan_reload(std::path::Path::new("/bundle"), &diff);
        // Added: LoadComponent + SeedSecrets
        // Removed: UnloadComponent
        // Changed: ReloadComponent
        // + UpdateRoutes
        assert_eq!(plan.actions.len(), 5);
    }

    #[test]
    fn empty_diff_no_actions() {
        let diff = BundleDiff::default();
        let plan = plan_reload(std::path::Path::new("/bundle"), &diff);
        assert!(plan.actions.is_empty());
    }
}
