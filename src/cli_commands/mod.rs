//! CLI command implementations for greentic-setup.

mod doctor;
mod inspect;
mod lifecycle;
mod setup;

pub use doctor::doctor;
pub use inspect::{build, list, status};
pub use lifecycle::{add, init, remove};
pub use setup::{
    execute_pending_oauth_device_actions, print_pending_setup_actions, setup, setup_migrate,
    setup_next, setup_reset, setup_retry, setup_status, update, wait_for_pending_oauth_callbacks,
};
