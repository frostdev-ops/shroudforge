// SPDX-License-Identifier: MIT
//! Kiln layer: when one package makes the startup pass fail, the pass runs once more without it.
//!
//! Upstream puts the game data back after a failed pass and starts the game with none of the packages
//! the pass writes. One package whose own check failed (a clash with another mod, say) then cost the
//! player all of them. Now a failure raised by one package's own entrypoint (`PregameModError`) runs
//! the pass once more, from the clean baseline as every pass does, with that package treated as
//! switched off (and with it the packages that need it). Once only: no loop, no search. A failure of
//! the system or of the game's files (an I/O error in the error's chain, whatever its text, or a
//! text with `os error` or access denied) is not one package's, and a failure no package raised
//! (baseline, staging, commit) names none: both keep upstream's behaviour.
//!
//! The record of the pass (state.json "assets") gets `skipped` (the package and its error, then the
//! packages left out with it) and, when the second pass failed too, `retryError`. Its `error` stays
//! the first failure's, as upstream records it.

use crate::LoaderError;
use shroudforge_package::ModEnvironment;
use std::path::Path;

/// The error of a package in a log line or the record is cut to this many characters.
const MAX_ERROR: usize = 400;

/// The package a failed pass is pinned on.
pub(super) struct FailedMod {
    id: String,
    name: String,
    version: String,
    /// Its error on one line, without the Lua stack traceback.
    error: String,
}

impl FailedMod {
    /// "Wisp Light Timer (wisp-light-timer 1.0.3)", or "wisp-light-timer 1.0.3" when the name is the id.
    fn label(&self) -> String {
        if self.name == self.id || self.name.is_empty() {
            format!("{} {}", self.id, self.version)
        } else {
            format!("{} ({} {})", self.name, self.id, self.version)
        }
    }
}

/// What the retry adds to the record of the pass.
#[derive(Default)]
pub(super) struct Retry {
    /// The failed package first, then the packages left out because they need it.
    pub(super) skipped: Vec<serde_json::Value>,
    /// The second pass's error, when it failed too.
    pub(super) error: Option<String>,
}

impl Retry {
    /// Adds the retry's fields to a record of the pass (nothing when there was no retry).
    pub(super) fn record_into(&self, record: &mut serde_json::Value) {
        if !self.skipped.is_empty() {
            record["skipped"] = serde_json::Value::Array(self.skipped.clone());
        }
        if let Some(error) = &self.error {
            record["retryError"] = error.as_str().into();
        }
    }
}

/// The error on one line: the Lua stack traceback cut off, white space collapsed, at most
/// [`MAX_ERROR`] characters.
fn one_line(text: &str) -> String {
    let text = text.split("stack traceback:").next().unwrap_or(text);
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > MAX_ERROR {
        return line.chars().take(MAX_ERROR).collect::<String>() + "...";
    }
    line
}

/// A failure of the system or of the game's files, not of one package: an I/O error in the error's
/// chain (`PregameModError::io`, whatever its text says), or, for one the API or the package put into
/// words, a text with Rust's "(os error N)" (in every language) or an access-denied text of Windows,
/// Wine or Unix.
fn system_failure(error: &shroudforge_api::PregameModError) -> bool {
    error.io || system_failure_text(&error.message)
}

fn system_failure_text(message: &str) -> bool {
    let lower = message.to_lowercase();
    [
        "(os error",
        "access denied",
        "access is denied",
        "permission denied",
    ]
    .iter()
    .any(|text| lower.contains(text))
}

/// The package a failed pass is pinned on: `error` is the error a package's own entrypoint raised,
/// it is no failure of the system or the game's files, and the package is in this pass's plan.
pub(super) fn failed_mod(
    error: Option<&shroudforge_api::PregameModError>,
    environment: &ModEnvironment,
    server: bool,
) -> Option<FailedMod> {
    let error = error?;
    if system_failure(error) {
        return None;
    }
    let plan = environment.plan(server, shroudforge_api::API_VERSION);
    let item = plan.iter().find(|item| item.info().id == error.mod_id)?;
    Some(FailedMod {
        id: error.mod_id.clone(),
        name: item.info().name.clone(),
        version: item.info().version.to_string(),
        error: one_line(&error.message),
    })
}

/// "the other 3 mods loaded", "the other mod loaded", "no other mod needs the pass".
fn others(count: usize) -> String {
    match count {
        0 => "no other mod needs the pregame pass".into(),
        1 => "the other mod loaded".into(),
        n => format!("the other {n} mods loaded"),
    }
}

/// Runs the pass once more without `failed`, after the first pass failed with `first`. `apply` is
/// one whole pass (baseline restore, the packages, the commit) for an environment, as the first pass
/// ran it. Returns the second pass's outcome (on failure: `first`, which the record and the caller
/// keep) and what the record gets.
pub(super) fn run_once_more(
    game_directory: &Path,
    environment: &ModEnvironment,
    server: bool,
    failed: FailedMod,
    first: LoaderError,
    apply: impl Fn(&ModEnvironment) -> Result<Vec<serde_json::Value>, LoaderError>,
) -> (Result<Vec<serde_json::Value>, LoaderError>, Retry) {
    let log = |message: &str| {
        let _ = shroudforge_logging::append(game_directory, 'W', "startup-assets", message);
    };
    log(&format!(
        "ShroudForge: {} failed in the pregame pass ({}); running the pass once more without it",
        failed.label(),
        failed.error
    ));
    let without = environment.without(&failed.id);
    // the packages that need the failed one drop out of the plan with it
    let pregame = |environment: &ModEnvironment| -> Vec<(String, String, String)> {
        environment
            .plan(server, shroudforge_api::API_VERSION)
            .into_iter()
            .filter(|item| item.info().requires_pregame())
            .map(|item| {
                (
                    item.info().id.clone(),
                    item.info().name.clone(),
                    item.info().version.to_string(),
                )
            })
            .collect()
    };
    let kept = pregame(&without);
    let dependents: Vec<_> = pregame(environment)
        .into_iter()
        .filter(|(id, ..)| *id != failed.id && !kept.iter().any(|(kept, ..)| kept == id))
        .collect();
    let mut retry = Retry::default();
    retry.skipped.push(serde_json::json!({
        "id": failed.id, "name": failed.name, "version": failed.version, "error": failed.error
    }));
    for (id, name, version) in &dependents {
        retry.skipped.push(serde_json::json!({
            "id": id, "name": name, "version": version,
            "error": format!("needs {}, which failed in the pregame pass", failed.id)
        }));
    }
    let with_it = if dependents.is_empty() {
        String::new()
    } else {
        format!(
            " and {} ({})",
            if dependents.len() == 1 {
                "the mod that needs it".to_string()
            } else {
                format!("the {} mods that need it", dependents.len())
            },
            dependents
                .iter()
                .map(|(id, ..)| id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    match apply(&without) {
        Ok(mods) => {
            log(&format!(
                "ShroudForge: {} failed in the pregame pass ({}); started the game without it{with_it}, {}",
                failed.label(),
                failed.error,
                others(mods.len())
            ));
            (Ok(mods), retry)
        }
        Err(second) => {
            log(&format!(
                "ShroudForge: the pregame pass failed again without {} ({}); the game starts without the mods of the pregame pass",
                failed.label(),
                one_line(&second.to_string())
            ));
            retry.error = Some(second.to_string());
            (Err(first), retry)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_error_is_one_line_without_the_traceback() {
        assert_eq!(
            one_line(
                "runtime error: [string \"wisp\"]:744: Missing base localization: keenglish\nstack traceback:\n\t[C]: in ?"
            ),
            "runtime error: [string \"wisp\"]:744: Missing base localization: keenglish"
        );
        assert_eq!(one_line("a\r\n  b\tc"), "a b c");
        assert_eq!(
            one_line(&"x".repeat(MAX_ERROR + 9)).chars().count(),
            MAX_ERROR + 3
        );
    }

    fn raised(message: &str, io: bool) -> shroudforge_api::PregameModError {
        shroudforge_api::PregameModError {
            mod_id: "wisp".into(),
            message: message.into(),
            io,
        }
    }

    #[test]
    fn failures_of_the_system_or_the_game_files_are_not_one_packages() {
        for message in [
            "runtime error: could not open enshrouded.kfc_resources: Access is denied. (os error 5)",
            "runtime error: Accès refusé. (os error 5)",
            "runtime error: The process cannot access the file because it is being used by another process. (os error 32)",
            "runtime error: Access denied",
            "runtime error: Permission denied",
        ] {
            assert!(system_failure(&raised(message, false)), "{message}");
        }
        // an I/O error whose text says none of that (a short read of a replaced archive, in any language)
        assert!(system_failure(&raised("failed to fill whole buffer", true)));
        assert!(!system_failure(&raised(
            "runtime error: [string \"wisp\"]:719: Missing base localization: keenglish",
            false
        )));
    }
}
