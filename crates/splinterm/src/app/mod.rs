//! Binary-owned application services.

mod access_cli;
mod access_monitor;
mod agent_status_cli;
mod approvals;
mod cli;
mod commands;
mod consent;
mod diagnostics_cli;
mod font_watch;
mod human_output;
mod integrations;
mod interrupted;
mod keymap_cli;
mod launcher_check;
mod local_service;
mod machine;
mod mcp_setup;
mod omarchy_hint;
mod omarchy_integration;
mod pane_bridge;
mod presets;
mod remote_chooser;
mod remote_cli;
mod remote_status;
mod session_catalog;
mod sessions;
mod shell_integration;
mod theme_watch;
mod topology_manager;
mod upgrade;
mod upgrade_dialog;
mod window;
mod window_resume;

pub(crate) use cli::run;
pub(crate) use remote_status::RemoteNotConnected;
pub(crate) use window_resume::run_resumed;
