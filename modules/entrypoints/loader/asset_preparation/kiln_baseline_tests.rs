// SPDX-License-Identifier: MIT
//! Kiln layer: the startup pass with a working baseline that is not the source of the game's files
//! (mod-runtime/startup/verify_working_baseline.rs), on a small, valid game archive.

use super::kiln_retry_tests::Game;
use super::*;
use kfc::{
    container::{KFCFile, KFCWriteOptions, KFCWriter},
    guid::ContentHash,
    reflection::TypeRegistry,
};
use std::{fs, path::Path, rc::Rc};

/// The fixture's version tag: its type cache belongs to it.
const VERSION: &str = "retry fixture";

fn options() -> KFCWriteOptions {
    KFCWriteOptions {
        overwrite_containers: true,
        truncate_containers: true,
        ..Default::default()
    }
}

/// A whole archive of its own in `dir` (as an update writes it), holding one content of `data`.
fn archive(dir: &Path, data: &[u8]) {
    fs::create_dir_all(dir).unwrap();
    for extension in ["kfc", "kfc_resources"] {
        fs::write(dir.join(format!("enshrouded.{extension}")), []).unwrap();
    }
    let mut writer = KFCWriter::<Rc<KFCFile>, _>::new_with_options(
        dir,
        "enshrouded",
        Rc::new(TypeRegistry::default()),
        VERSION,
        options(),
    )
    .unwrap();
    writer.write_content(&ContentHash::from_data(data), data).unwrap();
    writer.finalize().unwrap();
}

/// The game's files become those in `dir`, written after everything there is now.
fn install(dir: &Path, game: &Path) {
    std::thread::sleep(std::time::Duration::from_millis(50));
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            fs::copy(entry.path(), game.join(entry.file_name())).unwrap();
        }
    }
}

fn read(game: &Game, name: &str) -> Vec<u8> {
    fs::read(game.0.join(name)).unwrap()
}

#[test]
fn a_game_update_that_keeps_the_version_tag_gets_a_new_clean_copy() {
    let game = Game::new("update");
    game.package("alpha", "return {}", &[]);
    run_startup(&game.0).unwrap();
    let old_header = read(&game, "enshrouded.kfc.bak");

    // Steam updates the game; the archive's version tag stays.
    let update = game.0.join("steam-update");
    archive(&update, b"content of the updated game");
    install(&update, &game.0);
    let (header, resources) = (read(&game, "enshrouded.kfc"), read(&game, "enshrouded.kfc_resources"));
    assert!(header != old_header);

    run_startup(&game.0).unwrap();
    let record = game.record();
    assert_eq!(record["status"], "applied", "{record}");
    assert!(read(&game, "enshrouded.kfc.bak") == header, "the new clean copy is the updated game's header");
    assert!(read(&game, "enshrouded.kfc_resources.bak") == resources, "and its resource file");
    let kept = game.0.join("shroudforge/backups/outdated-working-baseline/enshrouded");
    let generations: Vec<_> = fs::read_dir(&kept).unwrap().map(|entry| entry.unwrap().path()).collect();
    assert_eq!(generations.len(), 1, "the old pair is kept aside");
    assert!(fs::read(generations[0].join("enshrouded.kfc.bak")).unwrap() == old_header);
    let log = game.log();
    assert!(
        log.contains("ShroudForge: its clean copy of the game data (enshrouded.kfc.bak, enshrouded.kfc_resources.bak) \
            was not used: the game's files are newer than it and were not built from it"),
        "{log}"
    );
    // The new pair is recorded: the next pass restores it.
    assert_eq!(
        shroudforge_game_assets::baseline_record::check(&game.0, "enshrouded").unwrap(),
        shroudforge_game_assets::baseline_record::Recorded::Whole
    );
}

#[test]
fn a_backup_that_does_not_fit_its_companion_never_goes_over_modded_files() {
    let game = Game::new("unfit");
    game.package("alpha", "return {}", &[]);
    // An earlier start's pair without a record, whose companion lost its data, and game files built over it.
    let vanilla = game.0.join("vanilla");
    archive(&vanilla, b"vanilla content");
    install(&vanilla, &game.0);
    for extension in ["kfc", "kfc_resources"] {
        fs::copy(game.0.join(format!("enshrouded.{extension}")), game.0.join(format!("enshrouded.{extension}.bak"))).unwrap();
    }
    fs::write(game.0.join("enshrouded.kfc_resources.bak"), []).unwrap();
    let base = Rc::new(KFCFile::from_path(game.0.join("enshrouded.kfc"), false).unwrap());
    let mut writer =
        KFCWriter::new_incremental_with_options(&game.0, "enshrouded", base, Rc::new(TypeRegistry::default()), options())
            .unwrap();
    writer.write_content(&ContentHash::from_data(b"a mod's content"), b"a mod's content").unwrap();
    writer.finalize().unwrap();
    let (header, resources) = (read(&game, "enshrouded.kfc"), read(&game, "enshrouded.kfc_resources"));

    let error = run_startup(&game.0).unwrap_err().to_string();
    assert!(error.contains("enshrouded.kfc.bak lists resource data up to byte"), "{error}");
    assert!(error.contains("Verify the game files in Steam"), "{error}");
    // Recovery refuses the same pair.
    assert!(!recover_failed_startup(&game.0, &error));
    assert!(read(&game, "enshrouded.kfc") == header, "the game's header is untouched");
    assert!(read(&game, "enshrouded.kfc_resources") == resources, "the game's resource file is untouched");
    assert!(read(&game, "enshrouded.kfc_resources.bak").is_empty(), "and the backups too");
}

#[test]
fn refusing_a_baseline_keeps_the_last_output_proof_for_the_next_start() {
    let game = Game::new("repeat-refusal");
    game.package("alpha", "return {}", &[]);
    run_startup(&game.0).unwrap();
    let replacement = game.0.join("unrelated-backup");
    archive(&replacement, b"another archive of the same version");
    for extension in ["kfc", "kfc_resources"] {
        fs::copy(replacement.join(format!("enshrouded.{extension}")),
            game.0.join(format!("enshrouded.{extension}.bak"))).unwrap();
    }
    let header = read(&game, "enshrouded.kfc");
    for _ in 0..2 {
        let error = run_startup(&game.0).unwrap_err().to_string();
        assert!(error.contains("Verify the game files in Steam"), "{error}");
        assert!(shroudforge_game_assets::prepared_stamp::left_live_header(&game.0, "enshrouded"));
        assert_eq!(read(&game, "enshrouded.kfc"), header);
    }
}

/// The loader configuration with `module`'s `toggleKey` set to `key` (as an earlier ShroudForge wrote it, or broken).
fn set_toggle_key(game: &Game, module: &str, key: serde_json::Value) {
    let path = shroudforge_loader_config::paths::loader_config(&game.0);
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["modules"][module]["toggleKey"] = key;
    fs::write(&path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
}

/// Steam's records (SteamCMD's layout: `steamapps` and `depotcache` in the game folder) listing the game files as they
/// are now, for the build `build`.
fn steam_lists_the_game_files(game: &Game, build: &str) {
    let files: Vec<(String, Vec<u8>)> = ["enshrouded.kfc", "enshrouded.kfc_resources"]
        .iter()
        .map(|name| (name.to_string(), read(game, name)))
        .collect();
    let listed: Vec<(&str, &[u8])> = files.iter().map(|(name, data)| (name.as_str(), data.as_slice())).collect();
    shroudforge_game_assets::steam_files::fixture::install_steam_records(&game.0.join("steamapps"), build, &listed);
}

fn outdated_generations(game: &Game) -> Vec<std::path::PathBuf> {
    let kept = game.0.join("shroudforge/backups/outdated-working-baseline/enshrouded");
    fs::read_dir(&kept).map(|entries| entries.map(|entry| entry.unwrap().path()).collect()).unwrap_or_default()
}

/// kiln-win1, 2026-11-25: a pass that fails before it reaches the working baseline (there: the loader configuration),
/// after a game update that left the working backups of the previous build. The recovery used to refuse to restore
/// those backups (right) and then failed the start, so no ShroudForge mod ran; it now takes the game's own files,
/// which the failed pass did not touch, as the new clean copy, and the game starts on them.
#[test]
fn a_pass_that_fails_after_a_game_update_takes_the_updated_files_as_the_new_clean_copy() {
    let game = Game::new("failed-after-update");
    game.package("alpha", "return {}", &[]);
    run_startup(&game.0).unwrap();
    let old_header = read(&game, "enshrouded.kfc.bak");
    let update = game.0.join("steam-update");
    archive(&update, b"content of the updated game");
    install(&update, &game.0);
    let (header, resources) = (read(&game, "enshrouded.kfc"), read(&game, "enshrouded.kfc_resources"));
    set_toggle_key(&game, "debugConsole", serde_json::json!("not a key"));

    let error = run_startup(&game.0).unwrap_err().to_string();
    assert!(error.contains("debugConsole.toggleKey"), "{error}");
    assert!(recover_failed_startup(&game.0, &error), "the game starts on its own files: {}", game.log());
    assert!(read(&game, "enshrouded.kfc") == header && read(&game, "enshrouded.kfc_resources") == resources,
        "the game's files are untouched");
    assert!(read(&game, "enshrouded.kfc.bak") == header, "the new clean copy is the updated game's header");
    assert!(read(&game, "enshrouded.kfc_resources.bak") == resources, "and its resource file");
    let generations = outdated_generations(&game);
    assert_eq!(generations.len(), 1, "the old pair is kept aside");
    assert!(fs::read(generations[0].join("enshrouded.kfc.bak")).unwrap() == old_header);
    let log = game.log();
    assert!(log.contains("the game's own files were kept as the new clean copy"), "{log}");
    let record = game.record();
    assert_eq!(record["status"], "failed", "{record}");
    assert!(record.get("recoveryError").is_none(), "{record}");
    // Review (0.2.0-dev.31.13): the record says the game's files were kept, not put back (Kiln's launcher words it so)
    assert_eq!(record["recovery"], "kept", "{record}");
    // The next start with a working configuration applies over the new pair.
    set_toggle_key(&game, "debugConsole", serde_json::json!("SDL:67"));
    run_startup(&game.0).unwrap();
    assert_eq!(game.record()["status"], "applied");
}

/// The same update, and the game files are not what Steam's manifest of the installed build lists (another program
/// rewrote the header since): the pass and its recovery refuse, the working backups (the only clean copy there is)
/// stay where they are, and the record says what the player does.
#[test]
fn files_that_are_not_steams_never_become_the_clean_copy_and_the_record_says_what_to_do() {
    let game = Game::new("not-steams");
    game.package("alpha", "return {}", &[]);
    run_startup(&game.0).unwrap();
    let old_pair = (read(&game, "enshrouded.kfc.bak"), read(&game, "enshrouded.kfc_resources.bak"));
    let update = game.0.join("steam-update");
    archive(&update, b"content of the updated game");
    install(&update, &game.0);
    steam_lists_the_game_files(&game, "26430625");
    let other = game.0.join("another-program");
    archive(&other, b"content another program wrote");
    install(&other, &game.0);
    let (header, resources) = (read(&game, "enshrouded.kfc"), read(&game, "enshrouded.kfc_resources"));

    let error = run_startup(&game.0).unwrap_err().to_string();
    assert!(error.contains("are not the files Steam installed for build 26430625"), "{error}");
    assert!(error.contains("Verify the game files in Steam"), "{error}");
    assert!(!recover_failed_startup(&game.0, &error));
    let record = game.record();
    let recovery = record["recoveryError"].as_str().unwrap_or_else(|| panic!("no recoveryError: {record}"));
    assert!(recovery.contains("Verify the game files in Steam"), "{recovery}");
    assert!(read(&game, "enshrouded.kfc") == header && read(&game, "enshrouded.kfc_resources") == resources,
        "the game's files are untouched");
    assert!(read(&game, "enshrouded.kfc.bak") == old_pair.0 && read(&game, "enshrouded.kfc_resources.bak") == old_pair.1,
        "the working backups stay");
    assert!(outdated_generations(&game).is_empty(), "nothing was set aside");
    let log = game.log();
    assert!(log.contains("Asset baseline recovery failed") && log.contains("Verify the game files in Steam"), "{log}");
}

/// The player's whole case: legacy toggle keys from an earlier ShroudForge in its configuration, working backups of
/// the previous build, the game files Steam installed for the new one. The hosted start converts the keys (what
/// ShroudForge's Updater does after an install), sets the old pair aside, takes the new one and applies.
#[test]
fn a_hosted_start_after_a_game_update_with_legacy_toggle_keys_applies() {
    let game = Game::new("win1");
    game.package("alpha", "return {}", &[]);
    run_startup(&game.0).unwrap();
    let update = game.0.join("steam-update");
    archive(&update, b"content of the updated game");
    install(&update, &game.0);
    steam_lists_the_game_files(&game, "26430625");
    let (header, resources) = (read(&game, "enshrouded.kfc"), read(&game, "enshrouded.kfc_resources"));
    // a Windows virtual-key code, as ShroudForge 1.7 kept it: the middle mouse button maps on every platform
    set_toggle_key(&game, "worldEditor", serde_json::json!(4));
    #[cfg(windows)]
    {
        set_toggle_key(&game, "debugConsole", serde_json::json!(121)); // F10
        set_toggle_key(&game, "modloaderUi", serde_json::json!(120)); // F9
    }
    assert!(run_startup(&game.0).unwrap_err().to_string().contains("toggleKey: Hotkey must be an SDL binding string"),
        "the configuration as kiln-win1 has it");

    crate::run_post_install_migrations(&game.0).unwrap();
    let config = shroudforge_loader_config::config::read_loader(&game.0).unwrap();
    assert_eq!(config["modules"]["worldEditor"]["toggleKey"], "Mouse:2", "{config}");
    #[cfg(windows)]
    for module in ["debugConsole", "modloaderUi"] {
        assert!(config["modules"][module]["toggleKey"].as_str().is_some_and(|key| key.starts_with("SDL:")), "{config}");
    }
    run_startup(&game.0).unwrap();
    let record = game.record();
    assert_eq!(record["status"], "applied", "{record}");
    assert!(read(&game, "enshrouded.kfc.bak") == header && read(&game, "enshrouded.kfc_resources.bak") == resources);
    assert_eq!(outdated_generations(&game).len(), 1, "the old pair is kept aside");
    let log = game.log();
    assert!(log.contains("Steam's manifest of build 26430625 lists the game's files as they are"), "{log}");
}

/// Review (0.2.0-dev.31.13): the disk fills while the new clean copy is made after a game update. The pass has set the
/// old pair aside; the copy of the header (9 MB) is done, the copy of the resource file (215 MB) fails. Neither the
/// failed pass nor its recovery (which tries the same again) may leave a header backup without its companion: the
/// next set-aside would move that one and remove the generation that holds the old pair.
#[test]
fn a_clean_copy_that_cannot_be_finished_leaves_no_half_pair_and_the_old_pair_stays_aside() {
    let game = Game::new("disk-full");
    game.package("alpha", "return {}", &[]);
    run_startup(&game.0).unwrap();
    let old_pair = (read(&game, "enshrouded.kfc.bak"), read(&game, "enshrouded.kfc_resources.bak"));
    let update = game.0.join("steam-update");
    archive(&update, b"content of the updated game");
    install(&update, &game.0);
    let (header, resources) = (read(&game, "enshrouded.kfc"), read(&game, "enshrouded.kfc_resources"));
    // what a full disk does to the copy of the resource file: it cannot be written
    let blocked = game.0.join("enshrouded.kfc_resources.bak.partial");
    fs::create_dir(&blocked).unwrap();

    let error = run_startup(&game.0).unwrap_err().to_string();
    assert!(error.contains("enshrouded.kfc_resources"), "{error}");
    assert!(!game.0.join("enshrouded.kfc.bak").exists(), "no header backup without its companion after the pass");
    assert!(recover_failed_startup(&game.0, &error), "the game starts on its own files: {}", game.log());
    assert!(!game.0.join("enshrouded.kfc.bak").exists(), "nor after the recovery");
    assert!(read(&game, "enshrouded.kfc") == header && read(&game, "enshrouded.kfc_resources") == resources,
        "the game's files are untouched");
    let kept_old = |game: &Game| outdated_generations(game).iter().any(|generation| {
        fs::read(generation.join("enshrouded.kfc.bak")).ok().as_ref() == Some(&old_pair.0)
            && fs::read(generation.join("enshrouded.kfc_resources.bak")).ok().as_ref() == Some(&old_pair.1)
    });
    assert!(kept_old(&game), "the old pair is still kept aside: {:?}", outdated_generations(&game));
    assert_eq!(game.record()["recovery"], "kept", "{}", game.record());

    // the disk has room again: the next start makes the copy and applies; the old pair is still aside
    fs::remove_dir(&blocked).unwrap();
    run_startup(&game.0).unwrap();
    assert_eq!(game.record()["status"], "applied", "{}", game.record());
    assert!(read(&game, "enshrouded.kfc.bak") == header && read(&game, "enshrouded.kfc_resources.bak") == resources);
    assert!(kept_old(&game), "the old pair is still kept aside: {:?}", outdated_generations(&game));
}
