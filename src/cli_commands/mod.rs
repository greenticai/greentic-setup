//! CLI command implementations for greentic-setup.

mod doctor;
mod inspect;
mod lifecycle;
mod setup;

pub use doctor::doctor;
pub use inspect::{build, list, status};
pub use lifecycle::{add, init, remove};
pub use setup::{setup, update};
