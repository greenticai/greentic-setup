//! CLI modules for greentic-setup.
//!
//! This module contains the CLI command implementations organized by domain:
//! - `bundle`: Bundle lifecycle management (init, add, setup, update, remove, build, list, status)
//! - `wizard`: Bundle wizard commands (apply from answer document)
//! - `pack_extract`: Pack extraction utilities (file://, oci://, repo://, store://)

pub mod bundle;
pub mod pack_extract;
pub mod wizard;

// Re-export commonly used types
pub use bundle::{
    BundleAddArgs, BundleBuildArgs, BundleInitArgs, BundleListArgs, BundleRemoveArgs,
    BundleSetupArgs, BundleStatusArgs,
};
pub use pack_extract::{
    copy_dir_contents, count_files_with_extension, determine_pack_domain, extract_pack_to_bundle,
    get_provider_id_from_pack_ref, handle_webchat_gui,
};
pub use wizard::{
    BundleWizardAnswers, LauncherAnswerDocument, LauncherAnswers, WizardAnswerDocument,
    WizardApplyArgs,
};
