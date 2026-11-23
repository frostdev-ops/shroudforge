// SPDX-License-Identifier: MIT
//! Kiln layer: what the log (and with it Kiln's kiln.log) says when a Rust panic stopped the startup asset pass.
//!
//! Upstream logged "Rust panic during startup asset preparation: <the panic's message>", which can run over several
//! lines, names neither the package that ran nor what happened for the player, and holds whatever the message holds.
//! Now the error line a player reads (and the record of the pass, as its error) is plain and holds one name at most:
//! that ShroudForge stopped preparing mod assets because of an internal error, and the name of the package whose
//! entrypoint ran when it happened (`shroudforge_api::PregameModPanic`, when the panic came from there). The panic's
//! message goes on an info line of its own, for a bug report, cut to one line, each path in it cut to its last two
//! components (`state/extra.rs:287`), so neither the build machine's folders nor a player's reach a log a player
//! shares. The recovery is upstream's (`recover_failed_startup`): the saved baseline is put back and the game starts
//! without the pass.

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

/// The info line after it: the panic's own message on one line, for a bug report, its paths cut short ([`short_paths`]).
pub(crate) fn startup_panic_detail(payload: &(dyn Any + Send)) -> String {
    let detail = match payload.downcast_ref::<shroudforge_api::PregameModPanic>() {
        Some(named) => named.detail.clone(),
        None => crate::panic_detail(payload),
    };
    format!(
        "Internal error detail, for a bug report: {}",
        one_line(&short_paths(&detail))
    )
}

/// `text` with each absolute path in it (`C:\...`, `\\server\...` or `\\?\...`, `/...`) cut to its last two components:
/// `C:\Users\player\Enshrouded\a.kfc` reads `Enshrouded\a.kfc`, and
/// `/home/builder/.cargo/registry/src/.../mlua-0.11.6/src/state/extra.rs:287:17` reads `state/extra.rs:287:17`. A path
/// starts at the text's start or after white space, an opening bracket, a quote, `=`, `,` or `;`, and ends at a control
/// character, a quote, `<`, `>`, `|`, a `:` before white space or the end, or where the next path starts; it may hold
/// spaces (`Program Files (x86)`). Text after its last separator stays, so a message that runs on keeps its words.
fn short_paths(text: &str) -> String {
    let c: Vec<char> = text.chars().collect();
    let separator = |at: usize| c.get(at).is_some_and(|&ch| ch == '\\' || ch == '/');
    let boundary =
        |at: usize| at == 0 || c[at - 1].is_whitespace() || "([{<\"'`=,;".contains(c[at - 1]);
    let starts = |at: usize| {
        boundary(at)
            && match c[at] {
                '\\' => c.get(at + 1) == Some(&'\\'),
                '/' => c.get(at + 1).is_some_and(|next| !next.is_whitespace()),
                drive => {
                    drive.is_ascii_alphabetic() && c.get(at + 1) == Some(&':') && separator(at + 2)
                }
            }
    };
    let ends = |at: usize| {
        c[at].is_control()
            || "\"'`<>|".contains(c[at])
            || (c[at] == ':' && c.get(at + 1).is_none_or(|next| next.is_whitespace()))
            || starts(at)
    };
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < c.len() {
        if !starts(at) {
            out.push(c[at]);
            at += 1;
            continue;
        }
        let mut end = at + 1;
        while end < c.len() && !ends(end) {
            end += 1;
        }
        // where each run of separators ends; the path's last two components follow the last run but one
        let mut runs = Vec::new();
        let mut i = at;
        while i < end {
            if separator(i) {
                while i < end && separator(i) {
                    i += 1;
                }
                runs.push(i);
            } else {
                i += 1;
            }
        }
        let keep = if runs.len() >= 2 {
            runs[runs.len() - 2]
        } else {
            at
        };
        out.extend(&c[keep..end]);
        at = end;
    }
    out
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
            "Internal error detail, for a bug report: failed to open game\\file.kfc left: 1 right: 2"
        );
        assert_eq!(
            startup_panic_reason(&String::from("boom")),
            "ShroudForge stopped preparing mod assets because of an internal error"
        );
    }

    #[test]
    fn the_detail_cuts_every_path_to_its_last_two_components() {
        let detail = |message: &str| startup_panic_detail(&message.to_owned());
        let said = |rest: &str| format!("Internal error detail, for a bug report: {rest}");
        // the build machine's source path of a panic location, and a player's game folder (with spaces)
        assert_eq!(
            detail(
                "at /home/builder/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/mlua-0.11.6/src/state/extra.rs:287:17: boom"
            ),
            said("at state/extra.rs:287:17: boom")
        );
        assert_eq!(
            detail(
                "cannot read C:\\Program Files (x86)\\Steam\\steamapps\\common\\Enshrouded\\enshrouded.kfc (os error 5)"
            ),
            said("cannot read Enshrouded\\enshrouded.kfc (os error 5)")
        );
        // Wine's drive, a verbatim path, forward slashes, and a path quoted the way Rust's Debug writes it
        assert_eq!(
            detail("Z:\\home\\deck\\.local\\share\\Steam\\game\\a.kfc: not found"),
            said("game\\a.kfc: not found")
        );
        assert_eq!(
            detail("\\\\?\\C:\\Users\\player\\Enshrouded\\shroudforge\\state.json"),
            said("shroudforge\\state.json")
        );
        assert_eq!(
            detail("D:/Games/Enshrouded/mods/x/mod.lua"),
            said("x/mod.lua")
        );
        assert_eq!(
            detail("left: \"C:\\\\Users\\\\player\\\\game\\\\b.kfc\", right: 2"),
            said("left: \"game\\\\b.kfc\", right: 2")
        );
        // two paths in one message, and one on each line
        assert_eq!(
            detail("copy /home/player/a/b.kfc to /tmp/x/y/c.kfc failed"),
            said("copy a/b.kfc to y/c.kfc failed")
        );
        assert_eq!(
            detail("first /srv/build/one/two.rs\nthen E:\\data\\three\\four.rs"),
            said("first one/two.rs then three\\four.rs")
        );
        // short or relative paths and slashes that are no path stay as they are
        for kept in [
            "used 1000194 slots (1/2 and/or a / b)",
            "[string \"mods/forged-and-found/mod.lua\"]:12: attempt to index a nil value",
            "state/extra.rs:287",
            "/one",
            "C:\\one",
            "see https://example.org/a/b/c",
        ] {
            assert_eq!(detail(kept), said(kept), "{kept}");
        }
    }
}
