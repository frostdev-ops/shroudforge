// SPDX-License-Identifier: MIT
//! Kiln layer: what the log (and with it Kiln's kiln.log) says when a Rust panic stopped the startup asset pass.
//!
//! Upstream logged "Rust panic during startup asset preparation: <the panic's message>", which can run over several
//! lines, names neither the package that ran nor what happened for the player, and holds whatever the message holds.
//! Now the error line a player reads (and the record of the pass, as its error) is plain and holds one name at most:
//! that ShroudForge stopped preparing mod assets because of an internal error, and the name of the package whose
//! entrypoint ran when it happened (`shroudforge_api::PregameModPanic`, when the panic came from there). The panic's
//! message goes on an info line of its own, for a bug report, cut to one line. The recovery is upstream's
//! (`recover_failed_startup`): the saved baseline is put back and the game starts without the pass.

use super::{recover_failed_startup, retry_without_failed_mod::one_line};
use std::{any::Any, path::Path};

/// The error line for a panic with this payload: no message, no version, no path; a package's name at most.
pub(crate) fn startup_panic_reason(payload: &(dyn Any + Send)) -> String {
    const STOPPED: &str = "ShroudForge stopped preparing mod assets because of an internal error";
    match payload.downcast_ref::<shroudforge_api::PregameModPanic>() {
        Some(named) => format!("{STOPPED} while running {}", named.package),
        None => STOPPED.to_owned(),
    }
}

/// The info line after it: the panic's own message on one line, for a bug report.
pub(crate) fn startup_panic_detail(payload: &(dyn Any + Send)) -> String {
    let detail = match payload.downcast_ref::<shroudforge_api::PregameModPanic>() {
        Some(named) => named.detail.clone(),
        None => crate::panic_detail(payload),
    };
    format!(
        "Internal error detail, for a bug report: {}",
        one_line(&detail)
    )
}

/// Logs the reason and its detail and restores the saved baseline. Returns whether the game may start (the recovery's
/// outcome).
pub(crate) fn report_startup_panic(game_directory: &Path, payload: &(dyn Any + Send)) -> bool {
    let reason = startup_panic_reason(payload);
    let _ = shroudforge_logging::append(game_directory, 'E', "startup-assets", &reason);
    let _ = shroudforge_logging::append(
        game_directory,
        'I',
        "startup-assets",
        &startup_panic_detail(payload),
    );
    recover_failed_startup(game_directory, &reason)
}

#[cfg(test)]
mod tests {
    use super::{startup_panic_detail, startup_panic_reason};

    #[test]
    fn the_reason_names_the_package_that_ran_and_nothing_else() {
        let named = shroudforge_api::PregameModPanic {
            package: "Forged and Found".into(),
            detail:
                "cannot create a Lua reference, out of auxiliary stack space (used 1000194 slots)"
                    .into(),
        };
        assert_eq!(
            startup_panic_reason(&named),
            "ShroudForge stopped preparing mod assets because of an internal error while running Forged and Found"
        );
        assert_eq!(
            startup_panic_detail(&named),
            "Internal error detail, for a bug report: cannot create a Lua reference, out of auxiliary stack space \
             (used 1000194 slots)"
        );
        // a panic outside any package's entrypoint, with a message over several lines that holds a path
        let message: &str =
            "failed to open C:\\Users\\player\\game\\file.kfc\n  left: 1\n right: 2";
        assert_eq!(
            startup_panic_reason(&message),
            "ShroudForge stopped preparing mod assets because of an internal error"
        );
        assert_eq!(
            startup_panic_detail(&message),
            "Internal error detail, for a bug report: failed to open C:\\Users\\player\\game\\file.kfc left: 1 right: 2"
        );
        assert_eq!(
            startup_panic_reason(&String::from("boom")),
            "ShroudForge stopped preparing mod assets because of an internal error"
        );
    }
}
