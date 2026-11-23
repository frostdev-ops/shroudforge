// SPDX-License-Identifier: MIT
//! Kiln layer: a startup pass that failed in one package's own entrypoint names that package.

use std::any::Any;

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

/// A Rust panic while a package's pregame entrypoint ran (`LuaModRunner::run`). The panic goes on with this as its
/// payload, so the loader's report of it (`entrypoints/loader/asset_preparation/report_startup_panic.rs`) can name the
/// package. A panic is ShroudForge's own failure, not an error of the package: nothing runs the pass again without it.
#[derive(Debug, Clone)]
pub struct PregameModPanic {
    /// The package's name as a player sees it ("Forged and Found"), or its id when it has no name: on one line, without
    /// control characters, at most 80 characters. Nothing else (no version, path or message).
    pub package: String,
    /// The panic's own message, for a bug report only: the line a player reads leaves it out.
    pub detail: String,
}

/// A package's name in a [`PregameModPanic`] is cut to this many characters.
const MAX_PACKAGE_NAME: usize = 80;

impl PregameModPanic {
    /// Runs a package's entrypoint; a panic in it unwinds on with a [`PregameModPanic`] for that package. mlua carries a
    /// panic in a Rust function the package called through Lua back out as the same panic (unless the package's own
    /// `pcall` caught it: the sandbox keeps mlua's default there).
    pub(crate) fn name_package<T>(id: &str, name: &str, entrypoint: impl FnOnce() -> T) -> T {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(entrypoint)) {
            Ok(value) => value,
            Err(payload) => std::panic::resume_unwind(Box::new(Self {
                package: package_name(id, name),
                detail: panic_text(&*payload),
            })),
        }
    }
}

/// The name a player knows the package by (its id when the name is blank), as one plain line: control characters and
/// runs of white space become one space, and a long name is cut to [`MAX_PACKAGE_NAME`] characters.
fn package_name(id: &str, name: &str) -> String {
    let plain = |text: &str| {
        text.chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut package = plain(name);
    if package.is_empty() {
        package = plain(id);
    }
    if package.chars().count() > MAX_PACKAGE_NAME {
        package = package.chars().take(MAX_PACKAGE_NAME).collect::<String>() + "...";
    }
    package
}

/// The message of a panic payload (a `&str` or a `String`, as `panic!` makes them).
fn panic_text(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(named) = payload.downcast_ref::<PregameModPanic>() {
        named.detail.clone()
    } else {
        "panic payload was not a string".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{PregameModError, PregameModPanic};
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

    #[test]
    fn a_panic_in_a_function_the_package_called_names_the_package() {
        let lua = mlua::Lua::new();
        let function = lua
            .create_function(|_, ()| -> mlua::Result<()> {
                panic!("cannot create a Lua reference, out of auxiliary stack space (used 1000194 slots)")
            })
            .unwrap();
        lua.globals().set("failing", function).unwrap();
        let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            PregameModPanic::name_package("forged-and-found", "Forged and Found", || {
                lua.load("local function walk() failing() end\nwalk()")
                    .exec()
            })
        }))
        .unwrap_err();
        let named = payload.downcast_ref::<PregameModPanic>().unwrap();
        assert_eq!(named.package, "Forged and Found");
        assert_eq!(
            named.detail,
            "cannot create a Lua reference, out of auxiliary stack space (used 1000194 slots)"
        );
        // a package without a name goes by its id; an entrypoint that does not panic returns as before
        let payload = std::panic::catch_unwind(|| {
            PregameModPanic::name_package("wisp", " ", || panic!("{}", 7))
        })
        .unwrap_err();
        let named = payload.downcast_ref::<PregameModPanic>().unwrap();
        assert_eq!(
            (named.package.as_str(), named.detail.as_str()),
            ("wisp", "7")
        );
        assert_eq!(PregameModPanic::name_package("wisp", "Wisp", || 5), 5);
    }

    #[test]
    fn the_package_name_is_one_plain_line() {
        let named = |name: &str| {
            let payload = std::panic::catch_unwind(|| {
                PregameModPanic::name_package("the-id", name, || panic!("boom"))
            })
            .unwrap_err();
            payload
                .downcast_ref::<PregameModPanic>()
                .unwrap()
                .package
                .clone()
        };
        assert_eq!(named("Forged\nand\r\n\tFound"), "Forged and Found");
        assert_eq!(named("Escape\u{1b}[31m red"), "Escape [31m red");
        assert_eq!(named(""), "the-id");
        let long = "x".repeat(super::MAX_PACKAGE_NAME + 20);
        assert_eq!(named(&long), "x".repeat(super::MAX_PACKAGE_NAME) + "...");
    }
}
