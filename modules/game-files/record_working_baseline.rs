// SPDX-License-Identifier: MIT
//! Kiln layer: the record of the working baseline, `<game>/<stem>.kfc.baseline.json`.
//!
//! Before every startup asset pass ShroudForge copies its working backups, `<stem>.kfc.bak` and
//! `<stem>.kfc_resources.bak`, over the game files and builds the mods on top of them. It took any header backup of
//! the game's version and archive format for the clean copy of the game's files, and any companion for that header's.
//! A player's backup header from before a game update (the version tag had not changed) went back over the updated
//! files, and the pass failed with "failed to fill whole buffer": the old header pointed past the end of the resource
//! file (0.2.0-dev.30.6 report).
//!
//! This record is written when the pair is made from the game files (or when a whole pair without a record is taken
//! up): each backup's size, last-write time and SHA-256, so that a later start can tell that both files are still the
//! pair it made, byte for byte. `mod-runtime/startup/verify_working_baseline.rs` decides what a start does with that.

use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const SCHEME: &str = "kiln-sf-baseline 1";
/// A record is a few hundred bytes; anything larger is not one.
const MAX_RECORD: u64 = 64 * 1024;

/// `<game>/<stem>.kfc.baseline.json`, beside the backups it describes.
pub fn record_path(root: &Path, stem: &str) -> PathBuf {
    root.join(format!("{stem}.kfc.baseline.json"))
}

/// The backups, as named in the record and in the game folder.
fn backups(stem: &str) -> [(&'static str, String); 2] {
    [
        ("header", format!("{stem}.kfc.bak")),
        ("resources", format!("{stem}.kfc_resources.bak")),
    ]
}

/// What the record says about the backups in the game folder now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// No record, one this runtime cannot read, or both backups were replaced since it was written (another program
    /// made a new pair): it proves nothing about the backups.
    Unrecorded,
    /// Both backups are the pair the record describes, byte for byte.
    Whole,
    /// The backups are not the pair the record describes (why, in words): one of them was replaced or removed without
    /// the other, or a backup changed its content while its size and time stayed.
    Broken(String),
}

/// Writes the record of the two backups as they are now. Both must be regular files. Flushed and renamed like
/// ShroudForge's state files.
pub fn record(root: &Path, stem: &str) -> Result<(), String> {
    let mut files = serde_json::Map::new();
    for (key, name) in backups(stem) {
        files.insert(key.into(), crate::prepared_stamp::with_hash(&root.join(name), None)?);
    }
    crate::config::write_json(
        &record_path(root, stem),
        &json!({
            "scheme": SCHEME,
            "target": stem,
            "madeAt": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
            "files": files,
        }),
    )
}

/// Removes the record (it describes a pair that is no longer in the game folder).
pub fn forget(root: &Path, stem: &str) -> Result<(), String> {
    match fs::remove_file(record_path(root, stem)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("{}: {error}", record_path(root, stem).display())),
    }
}

/// When the record was written (seconds since 1970), if there is one this runtime reads.
pub fn made_at(root: &Path, stem: &str) -> Option<u64> {
    read(root, stem)?["madeAt"].as_u64()
}

fn read(root: &Path, stem: &str) -> Option<Value> {
    let path = record_path(root, stem);
    let metadata = fs::symlink_metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_RECORD {
        return None;
    }
    let value: Value = serde_json::from_slice(&fs::read(&path).ok()?).ok()?;
    (value["scheme"] == SCHEME && value["target"] == stem).then_some(value)
}

/// Compares the backups with the record. A backup whose size and last-write time are still the recorded ones is read
/// in full and compared by its SHA-256; one whose size or time changed was replaced. `Err` only when a backup that
/// has to be read cannot be.
pub fn check(root: &Path, stem: &str) -> Result<Recorded, String> {
    let Some(record) = read(root, stem) else {
        return Ok(Recorded::Unrecorded);
    };
    let mut kept = Vec::new();
    let mut replaced = Vec::new();
    for (key, name) in backups(stem) {
        let then = &record["files"][key];
        let now = crate::prepared_stamp::identity(&root.join(&name))?;
        if !now.is_null() && then["size"] == now["size"] && then["modified"] == now["modified"] {
            kept.push((key, name));
        } else {
            replaced.push((name, now.is_null()));
        }
    }
    match (kept.as_slice(), replaced.as_slice()) {
        ([], _) => Ok(Recorded::Unrecorded),
        ([(_, kept)], [(other, missing)]) => Ok(Recorded::Broken(if *missing {
            format!("{other}, made together with {kept}, is missing")
        } else {
            format!("{other} was replaced since it was made together with {kept}")
        })),
        _ => {
            for (key, name) in kept {
                let now = crate::prepared_stamp::with_hash(&root.join(&name), None)?;
                if now["sha256"] != record["files"][key]["sha256"] {
                    return Ok(Recorded::Broken(format!(
                        "{name} changed since it was made (its content differs from the record, its size and time do not)"
                    )));
                }
            }
            Ok(Recorded::Whole)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Folder(PathBuf);
    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn folder() -> Folder {
        let root = std::env::temp_dir().join(format!("sf-baseline-record-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("enshrouded.kfc.bak"), b"header backup").unwrap();
        fs::write(root.join("enshrouded.kfc_resources.bak"), b"resources backup").unwrap();
        Folder(root)
    }

    /// Writes new content and gives the file its old last-write time back.
    fn rewrite_keeping_time(path: &Path, content: &[u8]) {
        let time = fs::metadata(path).unwrap().modified().unwrap();
        fs::write(path, content).unwrap();
        fs::File::options().write(true).open(path).unwrap().set_modified(time).unwrap();
    }

    #[test]
    fn a_recorded_pair_is_whole_until_one_backup_changes() {
        let game = folder();
        let root = &game.0;
        assert_eq!(check(root, "enshrouded").unwrap(), Recorded::Unrecorded);
        record(root, "enshrouded").unwrap();
        assert_eq!(check(root, "enshrouded").unwrap(), Recorded::Whole);
        assert!(made_at(root, "enshrouded").is_some());

        // Same size and time, other bytes: the content is compared.
        rewrite_keeping_time(&root.join("enshrouded.kfc_resources.bak"), b"resources BACKUP");
        assert!(matches!(check(root, "enshrouded").unwrap(),
            Recorded::Broken(why) if why.contains("enshrouded.kfc_resources.bak changed since it was made")));
    }

    #[test]
    fn one_backup_replaced_or_missing_is_not_the_pair() {
        let game = folder();
        let root = &game.0;
        record(root, "enshrouded").unwrap();
        fs::write(root.join("enshrouded.kfc_resources.bak"), b"resources of another game version").unwrap();
        assert_eq!(
            check(root, "enshrouded").unwrap(),
            Recorded::Broken(
                "enshrouded.kfc_resources.bak was replaced since it was made together with enshrouded.kfc.bak".into()
            )
        );
        fs::remove_file(root.join("enshrouded.kfc_resources.bak")).unwrap();
        assert_eq!(
            check(root, "enshrouded").unwrap(),
            Recorded::Broken("enshrouded.kfc_resources.bak, made together with enshrouded.kfc.bak, is missing".into())
        );
    }

    #[test]
    fn a_pair_another_program_made_anew_is_unrecorded_and_other_targets_are_not_read() {
        let game = folder();
        let root = &game.0;
        record(root, "enshrouded").unwrap();
        fs::write(root.join("enshrouded.kfc.bak"), b"a new header backup").unwrap();
        fs::write(root.join("enshrouded.kfc_resources.bak"), b"a new resources backup").unwrap();
        assert_eq!(check(root, "enshrouded").unwrap(), Recorded::Unrecorded);

        // A record of the other target, or of another scheme, is no record of this pair.
        fs::rename(record_path(root, "enshrouded"), record_path(root, "enshrouded_server")).unwrap();
        fs::copy(record_path(root, "enshrouded_server"), record_path(root, "enshrouded")).unwrap();
        assert_eq!(check(root, "enshrouded").unwrap(), Recorded::Unrecorded, "the record names the other target");
        fs::write(record_path(root, "enshrouded"), br#"{"scheme":"other","target":"enshrouded"}"#).unwrap();
        assert_eq!(check(root, "enshrouded").unwrap(), Recorded::Unrecorded);
        forget(root, "enshrouded").unwrap();
        assert!(!record_path(root, "enshrouded").exists());
        forget(root, "enshrouded").unwrap();
    }
}
