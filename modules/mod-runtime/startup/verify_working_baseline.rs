// SPDX-License-Identifier: MIT
//! Kiln layer: whether the working baseline may go back over the game files before a startup asset pass.
//!
//! ShroudForge restores `<stem>.kfc.bak` and `<stem>.kfc_resources.bak` over the game files before every pass. It
//! checked only that the header backup has the game's version tag and archive format (`validate_backup`). A player's
//! header backup from before a game update that kept the version tag went back over the updated files, and the pass
//! failed with "failed to fill whole buffer": the old header pointed past the end of the resource file
//! (0.2.0-dev.30.6 report). Nothing checked that the companion belongs to the header either.
//!
//! Now the pair goes back only when
//! - it is whole: both backups are the pair `<stem>.kfc.baseline.json` recorded when it was made (size and SHA-256,
//!   `game-files/record_working_baseline.rs`), or, for a pair without a record, the header backup's resource data
//!   fits in the companion; and
//! - it is the source of the game's files: the live header is the header backup, or was built over it. Every archive
//!   writer this runtime and Kiln use (kfc's `KFCWriter::new_incremental`) keeps the baseline's containers, contents
//!   and resource chunks where they were and only appends, so a header built over the backup lists all of those
//!   unchanged. A game update or Steam's file check writes a header of its own.
//!
//! When the backups are not the source of the game's files, the game's files are a game archive of their own (an
//! update, Steam's file check): the old pair is set aside and a new one is made from them. When the game's files were
//! built from a pair that is not whole, or are what this runtime's last pass wrote, neither the pair nor the game's
//! files are a clean copy: the start refuses and says to verify the game files.

use kfc::container::KFCFile;
use shroudforge_game_assets::baseline_record::{self, Recorded};
use std::{fs, path::Path, time::SystemTime};

/// What a start does with the working baseline before its pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Baseline {
    /// There is no header backup: the pass makes the pair from the game's files, as ShroudForge's first pass does.
    Missing,
    /// The pair goes back over the game's files. `recorded`: the record already describes it (otherwise the pair is
    /// recorded once it is restored).
    Restore { recorded: bool },
    /// The pair is not the source of the game's files, which are a game archive of their own: set the pair aside and
    /// make a new one from them (why, in words).
    Retake(String),
    /// Neither the pair nor the game's files are a clean copy (why, in words, with what the player does).
    Refuse(String),
}

/// What to do with the working baseline of `stem` in `game_dir`. `live_is_last_output`: the live header is the one
/// the last successful pass of this runtime left (`prepared_stamp::left_live_header`, read before the pass removes
/// that record), so it is never taken for a clean copy.
pub fn check_working_baseline(
    game_dir: impl AsRef<Path>,
    stem: &str,
    live_is_last_output: bool,
) -> Baseline {
    let game = game_dir.as_ref();
    let kfc = game.join(format!("{stem}.kfc"));
    let resources = game.join(format!("{stem}.kfc_resources"));
    let bak = game.join(format!("{stem}.kfc.bak"));
    let resources_bak = game.join(format!("{stem}.kfc_resources.bak"));
    let (bak_name, resources_bak_name) = (format!("{stem}.kfc.bak"), format!("{stem}.kfc_resources.bak"));
    match fs::symlink_metadata(&bak) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let live = KFCFile::from_path(&kfc, false).ok();
            return match live {
                Some(live) => match replacement(&resources, &live, stem, live_is_last_output, format!("{bak_name} is missing")) {
                    Baseline::Retake(_) => Baseline::Missing,
                    other => other,
                },
                None => Baseline::Refuse(refusal(stem, &format!("{bak_name} is missing"), "cannot be read")),
            };
        }
        Err(error) => return Baseline::Refuse(format!("{bak_name} cannot be read: {error}")),
        Ok(metadata) if !metadata.is_file() => {
            return Baseline::Refuse(format!("{bak_name} is not a file; move it out of the game folder"));
        }
        Ok(_) => {}
    }
    let live = KFCFile::from_path(&kfc, false).ok();
    let base = KFCFile::from_path(&bak, false).ok();

    // ShroudForge's own rule first: a backup of another game version or archive format is replaced.
    let incompatible = match (camino::Utf8Path::from_path(&kfc), camino::Utf8Path::from_path(&bak)) {
        (Some(kfc), Some(bak)) => !super::manage_asset_baseline::validate_backup(kfc, bak),
        _ => false,
    };
    if live.is_some() && incompatible {
        let why = match (&base, &live) {
            (None, _) => format!("{bak_name} is not a readable game archive"),
            (Some(base), Some(live)) if base.game_version() != live.game_version() => format!(
                "it is of game version {}, the game's files are of {}",
                base.game_version(),
                live.game_version()
            ),
            _ => "it is in another archive format than the game's files".into(),
        };
        return replacement(&resources, live.as_ref().unwrap(), stem, live_is_last_output, why);
    }
    let Some(base) = base else {
        return Baseline::Refuse(refusal(stem, &format!("{bak_name} is not a readable game archive"), "cannot be read"));
    };

    // Is the pair whole?
    let recorded = match baseline_record::check(game, stem) {
        Ok(recorded) => recorded,
        Err(error) => return Baseline::Refuse(format!("the clean copy cannot be read: {error}")),
    };
    let broken = match &recorded {
        Recorded::Whole => None,
        Recorded::Broken(why) => Some(why.clone()),
        Recorded::Unrecorded => {
            let end = chunks_end(&base);
            match (length(&resources_bak), length(&resources)) {
                (Some(have), _) if end > have => Some(format!(
                    "{bak_name} lists resource data up to byte {end}, {resources_bak_name} has {have} bytes"
                )),
                (None, _) => Some(format!("{resources_bak_name} is missing or is not a regular file")),
                _ => None,
            }
        }
    };

    let Some(live) = live else {
        // The game's header does not read: only the pair can mend it.
        return match broken {
            None => Baseline::Restore { recorded: recorded == Recorded::Whole },
            Some(why) => Baseline::Refuse(refusal(stem, &why, "cannot be read")),
        };
    };
    let derived = built_over(&live, &base);
    let same = derived && built_over(&base, &live) && same_bytes(&kfc, &bak);
    match broken {
        None if derived => return Baseline::Restore { recorded: recorded == Recorded::Whole },
        Some(why) if derived && !same => {
            return Baseline::Refuse(refusal(stem, &why, "carry mods built from it"));
        }
        _ => {}
    }
    // The pair is not the source of the game's files (or not whole while the game's header is its header): the game's
    // files are taken as the new clean copy, when they are a whole archive no pass of this runtime wrote.
    let why = match broken {
        Some(why) => format!("it is not whole ({why})"),
        None if newer(&kfc, &bak) => {
            "the game's files are newer than it and were not built from it (the game was updated, or its files were \
             verified in Steam)"
                .into()
        }
        None => "the game's files were not built from it".into(),
    };
    replacement(&resources, &live, stem, live_is_last_output, why)
}

/// Every path that takes a new baseline must prove the same facts, including missing/incompatible backups.
fn replacement(resources: &Path, live: &KFCFile, stem: &str, live_is_last_output: bool, why: String) -> Baseline {
    if live_is_last_output {
        return Baseline::Refuse(refusal(stem, &why, "are what ShroudForge's last start prepared"));
    }
    let Some(live_length) = length(resources) else {
        return Baseline::Refuse(refusal(stem, &why, "have no regular companion resource file"));
    };
    if chunks_end(&live) > live_length {
        return Baseline::Refuse(format!(
            "{why}; and the game's own {stem}.kfc lists resource data up to byte {}, its {stem}.kfc_resources has {} \
             bytes. Verify the game files in Steam (or update the server files with SteamCMD's validate), then start \
             again",
            chunks_end(&live),
            live_length
        ));
    }
    Baseline::Retake(why)
}

/// The text of a refusal: why the pair cannot go back, what the game's files are, and what the player does.
fn refusal(stem: &str, why: &str, live: &str) -> String {
    format!(
        "the clean copy of the game data ({stem}.kfc.bak, {stem}.kfc_resources.bak) cannot be used: {why}; and the \
         game's current files {live}, so no new copy can be made from them. Verify the game files in Steam (or update \
         the server files with SteamCMD's validate), then start again"
    )
}

/// The end of the last byte the header's resource chunks read from the resource file.
fn chunks_end(header: &KFCFile) -> u64 {
    header
        .resource_chunks()
        .iter()
        .map(|chunk| chunk.offset.saturating_add(chunk.compressed_size))
        .max()
        .unwrap_or(0)
}

fn length(path: &Path) -> Option<u64> {
    fs::symlink_metadata(path).ok().filter(fs::Metadata::is_file).map(|metadata| metadata.len())
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

/// The game's header was written after the header backup (a copy keeps or takes the time of its writing).
fn newer(kfc: &Path, bak: &Path) -> bool {
    matches!((modified(kfc), modified(bak)), (Some(live), Some(base)) if live > base)
}

fn same_bytes(a: &Path, b: &Path) -> bool {
    matches!((fs::read(a), fs::read(b)), (Ok(a), Ok(b)) if a == b)
}

/// `outer` is `base` or was built over it: the same version tag, and `base`'s containers, resource chunks and
/// contents where `base` has them (an incremental write only appends containers and chunks and adds contents).
fn built_over(outer: &KFCFile, base: &KFCFile) -> bool {
    fn prefix<T>(base: &[T], outer: &[T], same: impl Fn(&T, &T) -> bool) -> bool {
        base.len() <= outer.len() && base.iter().zip(outer).all(|(a, b)| same(a, b))
    }
    outer.game_version() == base.game_version()
        && prefix(base.containers(), outer.containers(), |a, b| (a.size, a.count) == (b.size, b.count))
        && prefix(base.resource_chunks(), outer.resource_chunks(), |a, b| {
            (a.offset, a.size, a.compressed_size, a.uncompressed_offset, a.uncompressed_size)
                == (b.offset, b.size, b.compressed_size, b.uncompressed_offset, b.uncompressed_size)
        })
        && base.contents().len() <= outer.contents().len()
        && base.contents().iter().all(|(id, entry)| {
            outer.contents().get(id).is_some_and(|other| {
                (other.offset, other.flags, other.container_index)
                    == (entry.offset, entry.flags, entry.container_index)
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kfc::{container::KFCWriter, guid::ContentHash, reflection::TypeRegistry};
    use std::{path::PathBuf, rc::Rc};

    const STEM: &str = "enshrouded";

    struct Folder(PathBuf);
    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn folder(name: &str) -> Folder {
        let root = std::env::temp_dir().join(format!("sf-verify-baseline-{name}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        Folder(root)
    }

    fn options() -> kfc::container::KFCWriteOptions {
        kfc::container::KFCWriteOptions {
            overwrite_containers: true,
            truncate_containers: true,
            ..Default::default()
        }
    }

    /// A small archive of one game version in `dir`, holding one content of `data`.
    fn archive(dir: &Path, version: &str, data: &[u8]) {
        for extension in ["kfc", "kfc_resources"] {
            fs::write(dir.join(format!("{STEM}.{extension}")), []).unwrap();
        }
        let mut writer = KFCWriter::<Rc<KFCFile>, _>::new_with_options(
            dir,
            STEM,
            Rc::new(TypeRegistry::default()),
            version,
            options(),
        )
        .unwrap();
        writer.write_content(&ContentHash::from_data(data), data).unwrap();
        writer.finalize().unwrap();
    }

    /// What a pass leaves: the archive in `dir` built over its own header, with one more content.
    fn mod_over(dir: &Path, data: &[u8]) {
        let base = Rc::new(KFCFile::from_path(dir.join(format!("{STEM}.kfc")), false).unwrap());
        let mut writer = KFCWriter::new_incremental_with_options(
            dir,
            STEM,
            base,
            Rc::new(TypeRegistry::default()),
            options(),
        )
        .unwrap();
        writer.write_content(&ContentHash::from_data(data), data).unwrap();
        writer.finalize().unwrap();
    }

    fn back_up(dir: &Path) {
        for extension in ["kfc", "kfc_resources"] {
            fs::copy(dir.join(format!("{STEM}.{extension}")), dir.join(format!("{STEM}.{extension}.bak"))).unwrap();
        }
    }

    /// The game files of `from` become those of `to` (an update or Steam's file check writes them later).
    fn install(from: &Path, to: &Path) {
        std::thread::sleep(std::time::Duration::from_millis(20));
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            if !name.ends_with(".bak") && entry.file_type().unwrap().is_file() {
                fs::copy(entry.path(), to.join(name)).unwrap();
            }
        }
    }

    #[test]
    fn a_whole_pair_goes_back_over_the_files_built_from_it() {
        let game = folder("restore");
        archive(&game.0, "game 1", b"vanilla content");
        back_up(&game.0);
        assert_eq!(check_working_baseline(&game.0, STEM, false), Baseline::Restore { recorded: false });
        baseline_record::record(&game.0, STEM).unwrap();
        mod_over(&game.0, b"content a mod added");
        assert_eq!(check_working_baseline(&game.0, STEM, true), Baseline::Restore { recorded: true });
        fs::remove_file(game.0.join(format!("{STEM}.kfc.bak"))).unwrap();
        assert_eq!(check_working_baseline(&game.0, STEM, false), Baseline::Missing);
    }

    #[test]
    fn a_pair_older_than_the_game_files_is_replaced_not_restored() {
        let game = folder("stale");
        let update = folder("stale-update");
        archive(&game.0, "game 1", b"content before the update");
        back_up(&game.0);
        baseline_record::record(&game.0, STEM).unwrap();
        mod_over(&game.0, b"content a mod added");
        // The update keeps the version tag; its archive is its own.
        archive(&update.0, "game 1", b"content after the update!");
        install(&update.0, &game.0);
        match check_working_baseline(&game.0, STEM, false) {
            Baseline::Retake(why) => assert!(why.contains("newer than it and were not built from it"), "{why}"),
            other => panic!("{other:?}"),
        }
        // Game files a pass of this runtime wrote are never the new copy.
        assert!(matches!(check_working_baseline(&game.0, STEM, true),
            Baseline::Refuse(why) if why.contains("are what ShroudForge's last start prepared")));
    }

    #[test]
    fn a_pair_that_is_not_whole_is_never_restored() {
        let game = folder("broken");
        archive(&game.0, "game 1", b"vanilla content");
        back_up(&game.0);
        baseline_record::record(&game.0, STEM).unwrap();
        // Another companion takes the place of this pair's (long enough for the header: only the record tells).
        fs::write(game.0.join(format!("{STEM}.kfc_resources.bak")), vec![1u8; 9000]).unwrap();
        // The game's files are the header backup's own: they become the new copy.
        match check_working_baseline(&game.0, STEM, false) {
            Baseline::Retake(why) => assert!(why.contains("it is not whole (enshrouded.kfc_resources.bak was replaced"), "{why}"),
            other => panic!("{other:?}"),
        }
        // Built from it, they carry mods: refused.
        mod_over(&game.0, b"content a mod added");
        match check_working_baseline(&game.0, STEM, false) {
            Baseline::Refuse(why) => {
                assert!(why.contains("carry mods built from it"), "{why}");
                assert!(why.contains("Verify the game files in Steam"), "{why}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_pair_without_a_record_must_fit_its_companion() {
        let game = folder("legacy");
        let update = folder("legacy-update");
        archive(&game.0, "game 1", b"vanilla content");
        back_up(&game.0);
        // A companion too short for its header: what "failed to fill whole buffer" read.
        fs::write(game.0.join(format!("{STEM}.kfc_resources.bak")), []).unwrap();
        mod_over(&game.0, b"content a mod added");
        match check_working_baseline(&game.0, STEM, false) {
            Baseline::Refuse(why) => assert!(why.contains("enshrouded.kfc_resources.bak has 0 bytes"), "{why}"),
            other => panic!("{other:?}"),
        }
        archive(&update.0, "game 1", b"content after the update!");
        install(&update.0, &game.0);
        assert!(matches!(check_working_baseline(&game.0, STEM, false), Baseline::Retake(why) if why.contains("has 0 bytes")));
        // A whole game archive of another version is replaced by ShroudForge's own rule.
        archive(&update.0, "game 2", b"content of the next version");
        install(&update.0, &game.0);
        assert!(matches!(check_working_baseline(&game.0, STEM, false),
            Baseline::Retake(why) if why == "it is of game version game 1, the game's files are of game 2"));
    }

    #[test]
    fn a_missing_companion_is_not_borrowed_from_modded_live_files() {
        let game = folder("missing-companion");
        archive(&game.0, "game 1", b"vanilla content");
        back_up(&game.0);
        mod_over(&game.0, b"modded content");
        fs::remove_file(game.0.join(format!("{STEM}.kfc_resources.bak"))).unwrap();
        assert!(matches!(check_working_baseline(&game.0, STEM, true), Baseline::Refuse(_)));
    }

    #[test]
    fn an_incompatible_backup_does_not_make_damaged_live_files_clean() {
        let game = folder("incompatible-damaged");
        let update = folder("incompatible-update");
        archive(&game.0, "game 1", b"old content");
        back_up(&game.0);
        archive(&update.0, "game 2", b"new content");
        install(&update.0, &game.0);
        fs::remove_file(game.0.join(format!("{STEM}.kfc_resources"))).unwrap();
        assert!(matches!(check_working_baseline(&game.0, STEM, false), Baseline::Refuse(_)));
    }

    #[test]
    fn losing_the_header_backup_does_not_make_the_last_output_clean() {
        let game = folder("lost-header");
        archive(&game.0, "game 1", b"vanilla content");
        back_up(&game.0);
        mod_over(&game.0, b"modded content");
        fs::remove_file(game.0.join(format!("{STEM}.kfc.bak"))).unwrap();
        assert!(matches!(check_working_baseline(&game.0, STEM, true), Baseline::Refuse(_)));
    }

    #[test]
    fn a_companion_copy_failure_does_not_publish_only_the_baseline_header() {
        let game = folder("restore-copy-failure");
        archive(&game.0, "game 1", b"vanilla content");
        back_up(&game.0);
        baseline_record::record(&game.0, STEM).unwrap();
        mod_over(&game.0, b"modded content");
        let header = fs::read(game.0.join(format!("{STEM}.kfc"))).unwrap();
        assert_eq!(check_working_baseline(&game.0, STEM, true), Baseline::Restore { recorded: true });
        // A source becomes unavailable after validation, before the second copy (also possible with locks or I/O errors).
        let backup = game.0.join(format!("{STEM}.kfc_resources.bak"));
        fs::remove_file(&backup).unwrap();
        fs::create_dir(&backup).unwrap();
        assert!(!super::super::manage_asset_baseline::restore_checked(
            camino::Utf8Path::from_path(&game.0).unwrap(), STEM));
        assert!(fs::read(game.0.join(format!("{STEM}.kfc"))).unwrap() == header,
            "a failed pair restore must leave the previous live header in place");
    }

    #[test]
    fn damaged_game_files_are_never_the_new_copy() {
        let game = folder("damaged");
        let update = folder("damaged-update");
        archive(&game.0, "game 1", b"content before the update");
        back_up(&game.0);
        archive(&update.0, "game 1", b"content after the update!");
        install(&update.0, &game.0);
        fs::write(game.0.join(format!("{STEM}.kfc_resources")), []).unwrap();
        assert!(matches!(check_working_baseline(&game.0, STEM, false),
            Baseline::Refuse(why) if why.contains("the game's own enshrouded.kfc lists resource data")));
    }
}
