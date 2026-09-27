//! The `#[tauri::command]` layer, split by domain so `main.rs` stays a thin
//! shell (state registration, app plumbing, tray, smoke boot).
//!
//! Tauri's `generate_handler!` resolves each entry via its defining module
//! (it generates `__cmd__<name>` wrappers there), so `main.rs` references the
//! commands as `commands::<submodule>::<command>`. No re-exports here — they
//! would put the commands in a path where the `__cmd__` wrappers don't exist.

pub mod agent;
pub mod context;
pub mod editor;
pub mod misc;
pub mod model;
pub mod session;
pub mod settings;