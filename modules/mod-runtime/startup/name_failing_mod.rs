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
    /// An I/O error (`std::io::Error`) is in the error's chain: reading or writing a file through the
    /// API failed (the game's archives, the exports), which is the system's or the files' failure, not
    /// the package's. Read from the error itself, so it holds whatever language the system's message
    /// is in, and for I/O errors without an OS code (a short read of a replaced archive).
    pub io: bool,
}

impl PregameModError {
    pub(crate) fn new(mod_id: &str, error: mlua::Error) -> Self {
        Self {
            mod_id: mod_id.to_owned(),
            message: error.to_string(),
            io: has_io_error(&error),
        }
    }
}

/// Whether a `std::io::Error` is anywhere in `error`: in mlua's own chain (callback, context,
/// external error), or in the source chain of an external error (an `anyhow` context, say).
fn has_io_error(error: &mlua::Error) -> bool {
    error.chain().any(|link| {
        let mut current = Some(link);
        while let Some(error) = current {
            if error.is::<std::io::Error>() {
                return true;
            }
            current = error.source();
        }
        false
    })
}

impl std::fmt::Display for PregameModError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PregameModError {}

#[cfg(test)]
mod tests {
    use super::PregameModError;
    use std::io::{Error, ErrorKind};

    /// What a package's entrypoint raises when it calls `failing` (a Rust function of the API).
    fn raised(failing: fn() -> mlua::Result<()>, lua_code: &str) -> PregameModError {
        let lua = mlua::Lua::new();
        let function = lua.create_function(move |_, ()| failing()).unwrap();
        lua.globals().set("failing", function).unwrap();
        let error = lua.load(lua_code).set_name("wisp").exec().unwrap_err();
        PregameModError::new("wisp", error)
    }

    #[test]
    fn an_io_error_from_the_api_is_known_by_its_type_not_its_text() {
        // a short read of an archive another program replaced: no OS code, no "denied"
        let error = raised(
            || Err(Error::new(ErrorKind::UnexpectedEof, "failed to fill whole buffer").into()),
            "local function check() failing() end\ncheck()",
        );
        assert!(error.io, "{error:?}");
        assert!(!error.message.contains("os error"), "{}", error.message);
        // access denied (its text is in the system's language), raised again by the package's pcall
        let error = raised(
            || Err(Error::from_raw_os_error(5).into()),
            "local ok, e = pcall(failing)\nif not ok then error(e) end",
        );
        assert!(error.io, "{error:?}");
        // behind a context that does not show it
        let error = raised(
            || {
                Err(mlua::Error::external(
                    anyhow::Error::from(Error::new(ErrorKind::InvalidData, "bad block"))
                        .context("could not read the resource"),
                ))
            },
            "failing()",
        );
        assert!(error.io, "{error:?}");
        assert!(!error.message.contains("bad block"), "{}", error.message);
    }

    #[test]
    fn a_packages_own_error_is_no_io_error() {
        let error = raised(|| Ok(()), "error('Missing base localization: keenglish')");
        assert!(!error.io, "{error:?}");
        let error = raised(
            || {
                Err(mlua::Error::external(
                    "resource type not found in current game registry",
                ))
            },
            "failing()",
        );
        assert!(!error.io, "{error:?}");
        // a package that turned an I/O error into its own text: only the text is left to read
        let error = raised(
            || Err(Error::from_raw_os_error(5).into()),
            "local ok, e = pcall(failing)\nif not ok then error('could not read: ' .. tostring(e)) end",
        );
        assert!(
            !error.io && error.message.contains("(os error 5)"),
            "{error:?}"
        );
    }
}
