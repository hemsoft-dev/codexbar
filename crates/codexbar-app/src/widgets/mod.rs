//! Widgets: the tiles drawn for them (`cards`, `host`) and, on Windows only, the widget provider (#94). Windows starts
//! `codexbar.exe -RegisterProcessAsComServer` when the Widgets board needs CodexBar's widgets; that process registers the provider class, answers the board's calls and pushes
//! cards built from `widgets.json`. It never opens a window or reads credentials, and it runs beside the tray app
//! (which writes the snapshot), so it skips the single-instance check.
//!
//! The class and the widget definition are declared in `packaging/AppxManifest.xml`; only packaged apps can be
//! widget providers. The bindings in `bindings.rs` are generated from the Windows App SDK's Widgets metadata by
//! `scripts/Update-WidgetBindings.ps1`.

#[cfg(windows)]
#[rustfmt::skip]
#[allow(clippy::all, clippy::pedantic, unsafe_op_in_unsafe_fn)]
mod bindings;
pub mod cards;
pub mod host;
#[cfg(windows)]
mod provider;

#[cfg(windows)]
pub use provider::{BoardSupport, SERVER_ARG, serve};
