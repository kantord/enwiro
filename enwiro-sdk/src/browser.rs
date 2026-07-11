//! Browser-extension integration.
//!
//! Browsers talk to native programs through "native messaging": the browser
//! spawns the executable named by a small manifest file in its config
//! directory and exchanges length-prefixed JSON messages with it over
//! stdio. [`framing`] owns that wire format; [`install`] owns writing the
//! per-browser host manifests (shared by `enw browser install` and the
//! daemon's idempotent auto-install at startup).

mod framing;
mod install;

pub use framing::{read_message, write_message};
pub use install::{InstallOutcome, NativeHostManifest, install, install_at, resolve_enw_binary};

/// Native messaging host name; the extension addresses the host by this
/// name, and the manifest file is named after it.
pub const NATIVE_HOST_NAME: &str = "ro.enwi.browser_host";

/// Extension IDs allowed to spawn the native host. The first is the
/// development (load-unpacked) ID, pinned by the `key` field in the
/// extension's manifest.json. A Chrome Web Store listing gets its own ID;
/// it is appended here once one exists.
pub const EXTENSION_IDS: &[&str] = &["fiigfehpoiaamipdficcboaljopdopcb"];
