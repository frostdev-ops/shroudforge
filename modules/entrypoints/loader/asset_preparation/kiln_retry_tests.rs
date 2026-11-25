// SPDX-License-Identifier: MIT
//! Kiln layer: the startup pass on a small, valid game archive with made-up packages, when one of them
//! fails (retry_without_failed_mod.rs).

use super::*;
use kfc::{
    container::{KFCFile, KFCWriter},
    reflection::TypeRegistry,
};
use serde_json::json;
use std::{fs, path::PathBuf, rc::Rc};

/// A game folder with a small archive pair and the type cache ShroudForge reads before it would extract
/// the types from the executable (`mod-runtime/reflection/load_type_registry.rs`).
pub(super) struct Game(pub(super) PathBuf);

impl Game {
    pub(super) fn new(name: &str) -> Game {
        let root = std::env::temp_dir().join(format!(
            "sf-kiln-retry-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let exe = b"test executable identity";
        fs::write(root.join("enshrouded.exe"), exe).unwrap();
        for extension in ["kfc", "kfc_resources"] {
            fs::write(root.join(format!("enshrouded.{extension}")), []).unwrap();
        }
        KFCWriter::<Rc<KFCFile>, _>::new(
            &root,
            "enshrouded",
            Rc::new(TypeRegistry::default()),
            "retry fixture",
        )
        .unwrap()
        .finalize()
        .unwrap();
        shroudforge_loader_config::config::initialize_loader_config(&root).unwrap();
        let cache = shroudforge_loader_config::paths::cache_dir(&root);
        fs::create_dir_all(&cache).unwrap();
        let mut registry = TypeRegistry::default();
        registry.version = KFCFile::get_version_tag(root.join("enshrouded.kfc")).unwrap();
        fs::write(
            cache.join("types-enshrouded.json"),
            serde_json::to_vec(&registry).unwrap(),
        )
        .unwrap();
        // load_type_registry.rs's identity of the executable the cached types belong to
        fs::write(
            cache.join("types-enshrouded.identity.json"),
            serde_json::to_vec(&json!({
                "schemaVersion": 2,
                "executableSha256": shroudforge_loader_config::config::revision(exe),
                "reflectionExtractor": "kfc-a404b15cde40fc7bd43d812bb460ce40d2a9d3b1"
            }))
            .unwrap(),
        )
        .unwrap();
        Game(root)
    }

    /// A switched-on package with a `patch` part whose entrypoint is `lua`, needing `needs`.
    pub(super) fn package(&self, id: &str, lua: &str, needs: &[&str]) {
        let package = shroudforge_loader_config::paths::mods_dir(&self.0).join(id);
        fs::create_dir_all(package.join("src")).unwrap();
        let dependencies: Vec<_> = needs
            .iter()
            .map(|id| json!({"id": id, "version": "^1.0.0"}))
            .collect();
        fs::write(
            package.join("mod.json"),
            serde_json::to_vec(&json!({
                "id": id, "name": format!("The {id} mod"), "version": "1.2.3", "capabilities": ["patch"],
                "dependencies": dependencies
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            package.join("extended.mod.json"),
            r#"{"schemaVersion":1,"enabled":true,"targets":["client","server"]}"#,
        )
        .unwrap();
        fs::write(package.join("src/mod.lua"), lua).unwrap();
    }

    /// A package whose own check fails, as Wisp Light Timer's did when another mod had changed the
    /// localization it looks for.
    fn broken(&self, id: &str, message: &str) {
        self.package(
            id,
            &format!("local function check()\n  error('{message}')\nend\ncheck()\nreturn {{}}"),
            &[],
        );
    }

    pub(super) fn record(&self) -> serde_json::Value {
        shroudforge_loader_config::config::read_document(
            &self.0,
            shroudforge_game_assets::documents::APPLIED,
        )
        .unwrap()
    }

    /// ShroudForge's log of this start (Kiln copies its lines into kiln.log).
    pub(super) fn log(&self) -> String {
        fs::read_to_string(shroudforge_loader_config::paths::current_log(&self.0))
            .unwrap_or_default()
    }
}

impl Drop for Game {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn ids(list: &serde_json::Value) -> Vec<&str> {
    list.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["id"].as_str())
                .collect()
        })
        .unwrap_or_default()
}

const RETRY: &str = "running the pass once more without it";

#[test]
fn one_failing_package_is_left_out_and_the_others_load() {
    let game = Game::new("one");
    game.package("alpha", "return {}", &[]);
    game.broken("broken", "Missing base localization: keenglish");
    game.package("needs-broken", "return {}", &["broken"]);
    game.package("zeta", "return {}", &[]);
    run_startup(&game.0).unwrap();
    let record = game.record();
    assert_eq!(record["status"], "applied", "{record}");
    assert_eq!(ids(&record["mods"]), ["alpha", "zeta"], "{record}");
    assert_eq!(
        record["skipped"][0],
        json!({"id": "broken", "name": "The broken mod", "version": "1.2.3",
            "error": "runtime error: [string \"broken\"]:2: Missing base localization: keenglish"}),
        "the package and its error on one line, without the traceback: {record}"
    );
    assert_eq!(
        record["skipped"][1],
        json!({"id": "needs-broken", "name": "The needs-broken mod", "version": "1.2.3",
            "error": "needs broken, which failed in the pregame pass"}),
        "{record}"
    );
    assert_eq!(record.get("retryError"), None);
    assert!(record["fingerprint"].is_string(), "{record}");
    let log = game.log();
    assert!(log.contains("ShroudForge: The broken mod (broken 1.2.3) failed in the pregame pass (runtime error: \
        [string \"broken\"]:2: Missing base localization: keenglish); started the game without it and the mod that needs it \
        (needs-broken), the other 2 mods loaded"), "{log}");
    // The data of the pass stand for this configuration: the next start keeps them, and the record.
    assert!(already_applied(&game.0, "enshrouded", false).unwrap());
    run_startup(&game.0).unwrap();
    assert_eq!(game.record(), record);
    // Switched off, the package is no longer skipped: the pass runs again and records no retry.
    fs::write(
        shroudforge_loader_config::paths::mods_dir(&game.0).join("broken/extended.mod.json"),
        r#"{"schemaVersion":1,"enabled":false,"targets":["client","server"]}"#,
    )
    .unwrap();
    assert!(!already_applied(&game.0, "enshrouded", false).unwrap());
    run_startup(&game.0).unwrap();
    let record = game.record();
    assert_eq!(
        (record["status"].as_str(), record.get("skipped")),
        (Some("applied"), None),
        "{record}"
    );
    assert_eq!(ids(&record["mods"]), ["alpha", "zeta"]);
}

#[test]
fn a_second_failure_starts_without_the_pass_and_records_both_errors() {
    let game = Game::new("two");
    game.package("alpha", "return {}", &[]);
    game.broken("broken-a", "first check failed");
    game.broken("broken-b", "second check failed");
    let error = run_startup(&game.0).unwrap_err().to_string();
    assert!(
        error.starts_with("pregame phase failed: runtime error: [string \"broken-a\"]:2: first check failed\nstack traceback:"),
        "the first failure's error, as upstream records it: {error}"
    );
    let record = game.record();
    assert_eq!(record["status"], "failed", "{record}");
    assert_eq!(record["error"].as_str(), Some(error.as_str()));
    assert_eq!(ids(&record["skipped"]), ["broken-a"], "{record}");
    let second = record["retryError"].as_str().unwrap();
    assert!(
        second.starts_with(
            "pregame phase failed: runtime error: [string \"broken-b\"]:2: second check failed"
        ),
        "{second}"
    );
    let log = game.log();
    assert!(log.contains(RETRY) && log.contains("ShroudForge: the pregame pass failed again without The broken-a mod (broken-a 1.2.3) \
        (pregame phase failed: runtime error: [string \"broken-b\"]:2: second check failed); the game starts without the mods of \
        the pregame pass"), "{log}");
    // The loader then puts the clean data back (embedded.rs): the record keeps both errors.
    assert!(recover_failed_startup(&game.0, &error));
    let recovered = game.record();
    assert_eq!(
        (
            &recovered["status"],
            &recovered["error"],
            &recovered["skipped"],
            &recovered["retryError"]
        ),
        (
            &record["status"],
            &record["error"],
            &record["skipped"],
            &record["retryError"]
        ),
        "{recovered}"
    );
    // Review (0.2.0-dev.31.13): and it says that the pair went back (Kiln's launcher words it so)
    assert_eq!(recovered["recovery"], "restored", "{recovered}");
    // and a recovery for another error (a panic) does not take them over
    assert!(recover_failed_startup(
        &game.0,
        "Rust panic during startup asset preparation: boom"
    ));
    let other = game.record();
    assert_eq!(
        (other.get("skipped"), other.get("retryError")),
        (None, None),
        "{other}"
    );
}

#[test]
fn access_denied_is_not_retried() {
    let game = Game::new("denied");
    game.package("alpha", "return {}", &[]);
    game.broken(
        "broken",
        "could not open enshrouded.kfc_resources: Access is denied. (os error 5)",
    );
    let error = run_startup(&game.0).unwrap_err().to_string();
    assert!(error.contains("(os error 5)"), "{error}");
    let record = game.record();
    assert_eq!(record["status"], "failed", "{record}");
    assert_eq!(
        (record.get("skipped"), record.get("retryError")),
        (None, None),
        "{record}"
    );
    assert!(!game.log().contains(RETRY), "{}", game.log());
}

#[test]
fn a_failure_no_package_raised_is_not_retried() {
    let game = Game::new("nobody");
    game.package("alpha", "return {}", &[]);
    game.broken("broken", "Missing base localization: keenglish");
    // the pass cannot open its game model before any package runs
    fs::remove_file(
        shroudforge_loader_config::paths::cache_dir(&game.0).join("types-enshrouded.json"),
    )
    .unwrap();
    let error = run_startup(&game.0).unwrap_err().to_string();
    assert!(error.contains("Failed to create AppState"), "{error}");
    let record = game.record();
    assert_eq!(record["status"], "failed", "{record}");
    assert_eq!(
        (record.get("skipped"), record.get("retryError")),
        (None, None),
        "{record}"
    );
    assert!(!game.log().contains(RETRY), "{}", game.log());
}

#[test]
fn a_pass_without_failures_records_no_retry() {
    let game = Game::new("good");
    game.package("alpha", "return {}", &[]);
    game.package("beta", "return {}", &["alpha"]);
    run_startup(&game.0).unwrap();
    let record = game.record();
    assert_eq!(record["status"], "applied", "{record}");
    assert_eq!(ids(&record["mods"]), ["alpha", "beta"]);
    assert_eq!(
        (record.get("skipped"), record.get("retryError")),
        (None, None),
        "{record}"
    );
    assert!(!game.log().contains(RETRY));
}

/// The package's files are inputs of the pass: updated or fixed, the next start runs the pass again
/// instead of keeping the data of the pass that ran without it.
#[test]
fn an_updated_or_fixed_package_runs_the_pass_again() {
    let game = Game::new("fixed");
    game.package("alpha", "return {}", &[]);
    game.broken("broken", "first check failed");
    run_startup(&game.0).unwrap();
    let record = game.record();
    assert_eq!(ids(&record["skipped"]), ["broken"], "{record}");
    assert!(already_applied(&game.0, "enshrouded", false).unwrap());
    // an update that still fails: the pass runs again and names the new error
    game.broken("broken", "the updated check failed");
    assert!(!already_applied(&game.0, "enshrouded", false).unwrap());
    run_startup(&game.0).unwrap();
    let record = game.record();
    assert_eq!(record["status"], "applied", "{record}");
    assert!(
        record["skipped"][0]["error"]
            .as_str()
            .is_some_and(|error| error.ends_with("the updated check failed")),
        "{record}"
    );
    // fixed: the pass runs again with it and records no retry
    game.package("broken", "return {}", &[]);
    assert!(!already_applied(&game.0, "enshrouded", false).unwrap());
    run_startup(&game.0).unwrap();
    let record = game.record();
    assert_eq!(
        (record["status"].as_str(), record.get("skipped")),
        (Some("applied"), None),
        "{record}"
    );
    let mut loaded = ids(&record["mods"]);
    loaded.sort_unstable();
    assert_eq!(loaded, ["alpha", "broken"], "{record}");
}

/// A package that needs the failed one through another drops out with both.
#[test]
fn packages_that_need_the_failed_one_through_another_drop_out_too() {
    let game = Game::new("chain");
    game.broken("broken", "check failed");
    game.package("middle", "return {}", &["broken"]);
    game.package("top", "return {}", &["middle"]);
    game.package("zeta", "return {}", &[]);
    run_startup(&game.0).unwrap();
    let record = game.record();
    assert_eq!(record["status"], "applied", "{record}");
    assert_eq!(ids(&record["mods"]), ["zeta"], "{record}");
    assert_eq!(ids(&record["skipped"])[0], "broken", "{record}");
    let mut left_out = ids(&record["skipped"])[1..].to_vec();
    left_out.sort_unstable();
    assert_eq!(left_out, ["middle", "top"], "{record}");
}

#[test]
fn a_panic_in_the_pass_leaves_one_readable_reason_and_the_saved_baseline() {
    // report_startup_panic.rs, as the loader's panic branch (embedded.rs) calls it
    let game = Game::new("panic");
    game.package("alpha", "return {}", &[]);
    run_startup(&game.0).unwrap();
    let before = game.log();
    let payload: Box<dyn std::any::Any + Send> = Box::new(shroudforge_api::PregameModPanic {
        package: "The alpha mod".into(),
        detail: "cannot create a Lua reference, out of auxiliary stack space\n(used 1000194 slots)"
            .into(),
    });
    assert!(report_startup_panic(&game.0, &*payload));
    let reason = "ShroudForge stopped preparing mod assets because of an internal error while running The alpha mod";
    let log = game.log();
    let added: Vec<&str> = log[before.len()..].lines().collect();
    let errors: Vec<&&str> = added
        .iter()
        .filter(|line| line.starts_with("[E "))
        .collect();
    assert_eq!(errors.len(), 1, "{log}");
    assert!(
        errors[0].ends_with(&format!("[startup-assets] {reason}")),
        "{log}"
    );
    // the panic's own message: an info line of its own, on one line, after the error line
    let detail = added
        .iter()
        .position(|line| {
            line.starts_with("[I ")
                && line.ends_with(
                    "[startup-assets] Internal error detail, for a bug report: cannot create a Lua reference, out \
                     of auxiliary stack space (used 1000194 slots)",
                )
        })
        .unwrap_or_else(|| panic!("{log}"));
    let error = added
        .iter()
        .position(|line| line.starts_with("[E "))
        .unwrap();
    assert!(error < detail, "{log}");
    assert!(
        added.last().unwrap().ends_with(
            "Asset mods failed; saved baseline restored. Continuing game startup without asset patches."
        ),
        "the fallback stays: {log}"
    );
    let record = game.record();
    assert_eq!(
        (record["status"].as_str(), record["error"].as_str()),
        (Some("failed"), Some(reason)),
        "{record}"
    );
    // the next start runs the pass again
    assert!(!already_applied(&game.0, "enshrouded", false).unwrap());
}
