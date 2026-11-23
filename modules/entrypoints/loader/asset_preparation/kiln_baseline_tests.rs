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
