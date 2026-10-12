#[cfg(winit)]
pub mod winit;

// Mnemos IDE: Windows shutdown/logoff handling; the pure part is tested everywhere.
#[cfg(any(windows, test))]
pub(crate) mod end_session;

pub use mnemos_ui_core::windowing::*;
#[cfg(target_os = "linux")]
pub use winit::WindowingSystem;
