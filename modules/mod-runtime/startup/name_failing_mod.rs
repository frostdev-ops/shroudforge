// SPDX-License-Identifier: MIT
//! Kiln layer: a startup pass that failed in one package's own entrypoint names that package.

/// The error a package's pregame entrypoint raised (`LuaModRunner::run`), with the package's id. It
/// reads as the error alone, as before, so the pass's recorded error and its log line are unchanged.
/// The loader takes the id to run the pass once more without that package
/// (`entrypoints/loader/asset_preparation/retry_without_failed_mod.rs`).
#[derive(Debug, Clone)]
pub struct PregameModError {
    /// The manifest id of the package whose entrypoint failed.
    pub mod_id: String,
    /// The error as the Lua runtime gave it, whole (with its stack traceback).
    pub message: String,
}

impl PregameModError {
    pub(crate) fn new(mod_id: &str, error: mlua::Error) -> Self {
        Self {
            mod_id: mod_id.to_owned(),
            message: error.to_string(),
        }
    }
}

impl std::fmt::Display for PregameModError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PregameModError {}
