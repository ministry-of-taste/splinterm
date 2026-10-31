#![forbid(unsafe_code)]

//! Graphical client mechanisms for Splinterm.
//!
//! Wayland objects, shared-memory canvases, font data, and glyph caches remain
//! client-owned. Live protocol snapshots are replaceable derived renderer state;
//! the daemon remains the terminal-state and shell-lifetime authority.

pub mod accessibility;
#[doc(hidden)]
pub mod automation;
pub mod background_effect;
mod box_drawing;
pub mod child_spawn;
pub mod clipboard_image;
mod color;
pub mod config;
pub mod diagnostics;
pub mod endpoint;
pub mod font_shaping;
pub mod frontend;
pub mod geometry;
pub mod handoff_control;
pub mod handoff_status;
pub mod keymap;
mod native_atspi;
pub mod navigation_projection;
pub mod osc52;
pub mod pane;
pub mod preset;
pub mod remote;
pub mod remote_askpass;
pub mod remote_launch;
pub mod remote_session;
pub mod remote_status;
pub mod renderer;
pub mod session_picker;
#[doc(hidden)]
pub mod tab;
pub mod theme;
pub mod viewport;
pub mod wayland;
pub mod window_continuation;

pub use frontend::{
    AccessConsentRenderer, AuthorityStatus, DialogIdentity, DialogLine, DialogLineRole,
    DialogStyle, DialogText, ExplorerContextTarget, FontUpdate, LairDirection,
    LairExplorerActivationTarget, LairPromptKind, LairPromptTarget, PerfTraceCorrelation,
    SelectorKind, SessionPickerCatalog, SessionPickerCreationTarget, SessionPickerDecision,
    SessionPickerItem, SessionPickerTarget, SessionPickerUi, TerminalGridLimits, ThemeUpdate,
    TrustedConsentUi, WindowCommand, WindowDojoIdentity, WindowOptions, WindowPaneOptions,
    WindowThemeColors, WindowTopologyCommand, WindowTopologyUpdate, WindowUpdate,
};
pub use wayland::{
    CloseSignal, PhaseEnd, WindowSurface, check_window_options, run as run_window, run_window_on,
};
