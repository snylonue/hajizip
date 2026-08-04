//! Platform-specific integration helpers (system default app, etc.).
//!
//! The GUI is cross-platform (Linux + macOS, x86_64/aarch64; Windows is
//! packaged from CI). Anything that differs per OS lives here behind `cfg`
//! so the rest of the crate stays platform-agnostic.

use std::path::Path;
use std::process::Command;

/// Open a file with the system's default application (fire-and-forget).
///
/// - Linux: `xdg-open`
/// - macOS: `open`
/// - Windows: `cmd /C start "" <path>`
///
/// The process is spawned and immediately detached; the caller never waits
/// on it, so the UI thread stays responsive.
pub fn open_with_default_app(path: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open").arg(path).spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(path).spawn()?;
    }
    #[cfg(target_os = "windows")]
    {
        // `start` is a cmd builtin; the empty string is the window title.
        Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .spawn()?;
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        return Err(anyhow::anyhow!(
            "opening external applications is not supported on this platform"
        ));
    }
    Ok(())
}
