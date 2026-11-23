// SPDX-License-Identifier: MIT
//! Kiln layer: what the log (and with it Kiln's kiln.log) says when a Rust panic stopped the startup asset pass.
//!
//! Upstream logged "Rust panic during startup asset preparation: <the panic's message>", which can run over several
//! lines and names neither the package that ran nor what the message means for the player. Now the log gets one line:
//! that the pass stopped on an internal error of ShroudForge, the package whose entrypoint ran when it happened
//! (`shroudforge_api::PregameModPanic`, when the panic came from there), and the message on one line. The recovery is
//! upstream's (`recover_failed_startup`): the saved baseline is put back and the game starts without the pass, and the
//! record of the pass keeps the same line as its error.

use super::{recover_failed_startup, retry_without_failed_mod::one_line};
use std::{any::Any, path::Path};

/// The line for a panic with this payload.
pub(crate) fn startup_panic_reason(payload: &(dyn Any + Send)) -> String {
    match payload.downcast_ref::<shroudforge_api::PregameModPanic>() {
        Some(named) => format!(
            "ShroudForge stopped its startup asset pass on an internal error while running {}: {}",
            named.package,
            one_line(&named.detail)
        ),
        None => format!(
            "ShroudForge stopped its startup asset pass on an internal error: {}",
            one_line(&crate::panic_detail(payload))
        ),
    }
}

/// Logs the reason once and restores the saved baseline. Returns whether the game may start (the recovery's outcome).
pub(crate) fn report_startup_panic(game_directory: &Path, payload: &(dyn Any + Send)) -> bool {
    let reason = startup_panic_reason(payload);
    let _ = shroudforge_logging::append(game_directory, 'E', "startup-assets", &reason);
    recover_failed_startup(game_directory, &reason)
}

#[cfg(test)]
mod tests {
    use super::startup_panic_reason;

    #[test]
    fn the_reason_is_one_line_and_names_the_package_that_ran() {
        let named = shroudforge_api::PregameModPanic {
            package: "Forged and Found (forged-and-found 0.16.1)".into(),
            detail:
                "cannot create a Lua reference, out of auxiliary stack space (used 1000194 slots)"
                    .into(),
        };
        assert_eq!(
            startup_panic_reason(&named),
            "ShroudForge stopped its startup asset pass on an internal error while running Forged and Found \
             (forged-and-found 0.16.1): cannot create a Lua reference, out of auxiliary stack space (used 1000194 slots)"
        );
        // a panic outside any package's entrypoint, with a message over several lines
        let message: &str = "assertion `left == right` failed\n  left: 1\n right: 2";
        assert_eq!(
            startup_panic_reason(&message),
            "ShroudForge stopped its startup asset pass on an internal error: assertion `left == right` failed left: 1 \
             right: 2"
        );
        assert_eq!(
            startup_panic_reason(&String::from("boom")),
            "ShroudForge stopped its startup asset pass on an internal error: boom"
        );
    }
}
