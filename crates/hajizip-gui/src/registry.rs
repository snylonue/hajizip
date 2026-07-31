//! The GUI's composition root: which formats the application supports.
//!
//! Per `architecture.md` §4.4 and §5.1, the abstract core does not enumerate
//! formats; the application (here, the GUI) references the concrete format
//! implementations it wants and registers them into a [`Registry`]. This is the
//! only place in the GUI that names concrete formats.
//!
//! `hajizip-core` provides formats incrementally. As each one lands, add a
//! single `.register_archive(..)` / `.register_codec(..)` line here. Until
//! then the registry is empty and the GUI degrades gracefully, presenting an
//! "unsupported format" message rather than crashing.

use hajizip_core::Registry;

/// Build the registry of all formats the GUI currently supports.
pub fn compose_registry() -> Registry {
    Registry::new()
    // Formats are registered here as core provides them, e.g.:
    //   .register_archive(hajizip_core::archive::zip::ZipFormat::new())
    //   .register_codec(hajizip_core::codec::gzip::GzipFormat::new())
}
