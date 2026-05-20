//! `origin-cli` library entry — exposes input/screen/tui modules for the
//! binary and for unit tests.

pub mod admin;
pub mod admin_url;
pub mod cli_def;
pub mod headless;
pub mod import;
pub mod input;
pub mod keyring_login;
pub mod plan_panel_wiring;
pub mod providers;
pub mod screen;
pub mod status;
pub mod trace_cmd;
pub mod tui;
pub mod tutorial;

pub use cli_def::main_cli;
