//! Deprecation stub: this crate was renamed to `enwiro-bridge-activitywatch`.
//!
//! Exiting non-zero also makes the enwiro daemon's bridge `metadata` probe
//! treat a leftover installed binary as capability-free and leave it alone.

use std::process::ExitCode;

fn main() -> ExitCode {
    eprintln!(
        "enwiro-bridge-i3-activitywatch is deprecated and does nothing.\n\
         It was renamed to enwiro-bridge-activitywatch, which works with any \
         enwiro adapter.\n\
         Migrate with:\n\
         \n\
         \x20   cargo uninstall enwiro-bridge-i3-activitywatch\n\
         \x20   cargo install enwiro-bridge-activitywatch\n"
    );
    ExitCode::FAILURE
}
