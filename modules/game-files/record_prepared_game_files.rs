// SPDX-License-Identifier: MIT
//! Kiln layer: the record of what the last startup asset pass left in the game folder.
//!
//! ShroudForge skipped a start's asset pass when state.json "assets" said `applied` with the
//! current preparation fingerprint (the enabled pregame packages, their files, the executable's
//! size and time). Two gaps made that guard both too weak and too strict:
//! - Nothing checked the game's own files. Another loader (kiln-eml, EML), Steam's file check or a
//!   restore could replace `<stem>.kfc` after the pass, and the record still said `applied`.
//! - A pregame package with the `export` capability (exports are on by default) forced the whole
//!   restore-and-commit on every start, because nothing said whether its exports were still there.
//!   On a Steam Deck that was over two minutes per start (dev29 inbox 50, 2026-11-17 report).
//!
//! This record closes both, the way kiln-eml's file cache does for its own runs (size, last-write
//! time and a content hash, recorded after the run and compared at the next start): it is written
//! next to the working backup (`<stem>.kfc.prepared.json`) after a pass succeeded, removed before a
//! pass starts, and the pass is skipped only when everything in it is still what it was:
//! - the preparation fingerprint and the export switch the pass ran with,
//! - Steam's build id of the installation, when Steam's manifest is there,
//! - `<stem>.kfc` (size, time, SHA-256 of all of it: about 9 MB) and `<stem>.kfc_resources` (size,
//!   time, SHA-256 of its first and last MiB: about 200 MB is too much to read every start),
//! - both working backups (size and time),
//! - when the pass ran pregame exporters: every entry of the export folder (path, kind, size,
//!   time), because an exporter may read, list or delete what is there.
//!
//! Anything that cannot be read, a record of another scheme, or any difference means "apply".

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

const SCHEME: &str = "kiln-sf-prepared 1";
/// The part of `<stem>.kfc_resources` hashed at each end.
const EDGE: u64 = 1 << 20;
/// An export folder with more entries than this is not listed: the pass runs as before.
const MAX_EXPORT_ENTRIES: usize = 50_000;

/// What the pass ran with, besides the game files.
pub struct PassInputs<'a> {
    /// `shroudforge_package::prepared::fingerprint` of the plan the pass applied.
    pub fingerprint: &'a str,
    /// loader config `exports.enabled` at the pass.
    pub exports_enabled: bool,
    /// The export folder, when the pass ran a pregame package with the `export` capability.
    pub exporters: Option<&'a Path>,
}

/// `<game>/<stem>.kfc.prepared.json`, beside `<stem>.kfc.bak`.
pub fn record_path(root: &Path, file_name: &str) -> PathBuf {
    root.join(format!("{file_name}.kfc.prepared.json"))
}

/// Removes the record. Called before a pass changes the game files, so an interrupted pass never
/// leaves a record that describes files that are no longer there.
pub fn forget(root: &Path, file_name: &str) -> Result<(), String> {
    match fs::remove_file(record_path(root, file_name)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "{}: {error}",
            record_path(root, file_name).display()
        )),
    }
}

/// Writes the record of the files as they are now (after a successful pass). Flushed and renamed
/// like the state file.
pub fn record(root: &Path, file_name: &str, inputs: &PassInputs) -> Result<(), String> {
    let stamp = capture(root, file_name, inputs)?;
    crate::config::write_json(&record_path(root, file_name), &stamp)
}

/// `Ok` when the record is there and every part of it still holds; otherwise `Err` says what did
/// not. Whatever cannot be read or checked is an `Err` too: the caller then applies the asset mods
/// as before.
pub fn current(root: &Path, file_name: &str, inputs: &PassInputs) -> Result<(), String> {
    let path = record_path(root, file_name);
    let bytes = fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let recorded: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if recorded["scheme"] != SCHEME {
        return Err("the record is of another scheme".into());
    }
    let now = capture(root, file_name, inputs)?;
    for key in [
        "scheme",
        "target",
        "fingerprint",
        "exportsEnabled",
        "build",
        "files",
        "exports",
    ] {
        if recorded.get(key) != now.get(key) {
            return Err(format!("{key} differs from the last pass"));
        }
    }
    Ok(())
}

fn capture(root: &Path, file_name: &str, inputs: &PassInputs) -> Result<Value, String> {
    let file = |suffix: &str| root.join(format!("{file_name}.{suffix}"));
    Ok(json!({
        "scheme": SCHEME,
        "target": file_name,
        "fingerprint": inputs.fingerprint,
        "exportsEnabled": inputs.exports_enabled,
        "build": steam_build(root, file_name),
        "files": {
            "kfc": with_hash(&file("kfc"), None)?,
            "kfcResources": with_hash(&file("kfc_resources"), Some(EDGE))?,
            "kfcBackup": identity(&file("kfc.bak"))?,
            "kfcResourcesBackup": identity(&file("kfc_resources.bak"))?,
        },
        "exports": match inputs.exporters {
            Some(dir) => Value::from(export_entries(dir)?),
            None => Value::Null,
        },
    }))
}

fn modified_nanos(metadata: &fs::Metadata) -> Result<String, String> {
    let time = metadata.modified().map_err(|error| error.to_string())?;
    // Before 1970 or unreadable: no stamp to trust.
    let since = time
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?;
    Ok(since.as_nanos().to_string())
}

/// Size and last-write time of a regular file; a missing file is recorded as missing.
fn identity(path: &Path) -> Result<Value, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(json!({
            "size": metadata.len(),
            "modified": modified_nanos(&metadata)?,
        })),
        Ok(_) => Err(format!("{} is not a regular file", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Value::Null),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// `identity` plus a SHA-256 of the whole file, or of its first and last `edge` bytes.
fn with_hash(path: &Path, edge: Option<u64>) -> Result<Value, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let mut file = fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut hash = Sha256::new();
    let mut take = |file: &mut fs::File, mut left: u64| -> Result<(), String> {
        let mut buffer = vec![0u8; 1 << 16];
        while left > 0 {
            let want = buffer.len().min(usize::try_from(left).unwrap_or(usize::MAX));
            let count = file
                .read(&mut buffer[..want])
                .map_err(|error| format!("{}: {error}", path.display()))?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
            left -= count as u64;
        }
        Ok(())
    };
    let length = metadata.len();
    match edge {
        Some(edge) if length > edge * 2 => {
            take(&mut file, edge)?;
            file.seek(SeekFrom::End(-(edge as i64)))
                .map_err(|error| format!("{}: {error}", path.display()))?;
            take(&mut file, edge)?;
        }
        _ => take(&mut file, length)?,
    }
    // The file must not have changed while it was read.
    let after = fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if after.len() != length || modified_nanos(&after)? != modified_nanos(&metadata)? {
        return Err(format!("{} changed while it was read", path.display()));
    }
    Ok(json!({
        "size": length,
        "modified": modified_nanos(&metadata)?,
        "sha256": format!("{:x}", hash.finalize()),
    }))
}

/// Every entry below the export folder, sorted: [path, kind, size, last-write time]. A missing
/// folder is an empty list. Links are listed, not followed.
fn export_entries(dir: &Path) -> Result<Vec<Value>, String> {
    let mut entries = Vec::new();
    match fs::symlink_metadata(dir) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Err(format!("{} is not a folder", dir.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
        Err(error) => return Err(format!("{}: {error}", dir.display())),
    }
    let mut pending = vec![dir.to_path_buf()];
    while let Some(folder) = pending.pop() {
        for entry in fs::read_dir(&folder).map_err(|error| format!("{}: {error}", folder.display()))? {
            let entry = entry.map_err(|error| format!("{}: {error}", folder.display()))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            let relative = path
                .strip_prefix(dir)
                .map_err(|error| error.to_string())?
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            let kind = if metadata.is_dir() {
                pending.push(path.clone());
                "folder"
            } else if metadata.is_file() {
                "file"
            } else {
                "other"
            };
            let size = if metadata.is_file() { metadata.len() } else { 0 };
            entries.push(json!([relative, kind, size, modified_nanos(&metadata)?]));
            if entries.len() > MAX_EXPORT_ENTRIES {
                return Err(format!(
                    "{} has more than {MAX_EXPORT_ENTRIES} entries",
                    dir.display()
                ));
            }
        }
    }
    entries.sort_by(|a, b| a[0].as_str().cmp(&b[0].as_str()));
    Ok(entries)
}

/// Steam's build id of the installation (`buildid` in `steamapps/appmanifest_<app>.acf`), or null
/// where Steam's manifest is not there (a server installed without Steam).
fn steam_build(root: &Path, file_name: &str) -> Value {
    let app = if file_name == "enshrouded_server" {
        "2278520"
    } else {
        "1203620"
    };
    let Some(steamapps) = root.parent().and_then(Path::parent) else {
        return Value::Null;
    };
    let Ok(text) = fs::read_to_string(steamapps.join(format!("appmanifest_{app}.acf"))) else {
        return Value::Null;
    };
    text.lines()
        .find_map(|line| {
            let mut parts = line.split('"').filter(|part| !part.trim().is_empty());
            (parts.next()?.eq_ignore_ascii_case("buildid"))
                .then(|| parts.next().map(str::to_owned))
                .flatten()
        })
        .map(Value::from)
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Game {
        _temp: TempDir,
        root: PathBuf,
        exports: PathBuf,
    }

    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn game() -> Game {
        let base = std::env::temp_dir().join(format!("sf-prepared-{}", uuid::Uuid::new_v4()));
        let root = base.join("steamapps/common/Enshrouded");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            base.join("steamapps/appmanifest_1203620.acf"),
            "\"AppState\"\n{\n\t\"appid\"\t\t\"1203620\"\n\t\"buildid\"\t\t\"26430625\"\n}\n",
        )
        .unwrap();
        fs::write(root.join("enshrouded.kfc"), b"patched header").unwrap();
        fs::write(root.join("enshrouded.kfc_resources"), vec![7u8; 3 * EDGE as usize]).unwrap();
        fs::write(root.join("enshrouded.kfc.bak"), b"original header").unwrap();
        fs::write(root.join("enshrouded.kfc_resources.bak"), b"original resources").unwrap();
        let exports = root.join("shroudforge/exports");
        fs::create_dir_all(exports.join("holster")).unwrap();
        fs::write(exports.join("holster/map.json"), b"{}").unwrap();
        Game {
            _temp: TempDir(base),
            root,
            exports,
        }
    }

    fn inputs<'a>(fingerprint: &'a str, exporters: Option<&'a Path>) -> PassInputs<'a> {
        PassInputs {
            fingerprint,
            exports_enabled: true,
            exporters,
        }
    }

    /// Rewrites a file with the same length and puts its last-write time back: only the hash can
    /// tell.
    fn rewrite_keeping_time(path: &Path, bytes: &[u8]) {
        let time = fs::metadata(path).unwrap().modified().unwrap();
        fs::write(path, bytes).unwrap();
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    fn later(path: &Path) {
        let time = fs::metadata(path).unwrap().modified().unwrap() + std::time::Duration::from_secs(5);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    #[test]
    fn an_unchanged_game_folder_matches_the_record() {
        let g = game();
        let exports = g.exports.clone();
        record(&g.root, "enshrouded", &inputs("f1", Some(&exports))).unwrap();
        assert_eq!(current(&g.root, "enshrouded", &inputs("f1", Some(&exports))), Ok(()));
        let recorded: Value =
            serde_json::from_slice(&fs::read(record_path(&g.root, "enshrouded")).unwrap()).unwrap();
        assert_eq!(recorded["build"], "26430625");
    }

    #[test]
    fn no_record_or_a_forgotten_record_means_apply() {
        let g = game();
        assert!(current(&g.root, "enshrouded", &inputs("f1", None)).is_err());
        record(&g.root, "enshrouded", &inputs("f1", None)).unwrap();
        forget(&g.root, "enshrouded").unwrap();
        assert!(current(&g.root, "enshrouded", &inputs("f1", None)).is_err());
        forget(&g.root, "enshrouded").unwrap();
    }

    #[test]
    fn any_change_means_apply() {
        type Change = fn(&Game);
        let changes: [(&str, Change); 12] = [
            ("kfc rewritten, same length and time", |g| {
                rewrite_keeping_time(&g.root.join("enshrouded.kfc"), b"vanilla header")
            }),
            ("kfc touched", |g| later(&g.root.join("enshrouded.kfc"))),
            ("kfc removed", |g| fs::remove_file(g.root.join("enshrouded.kfc")).unwrap()),
            ("resources start rewritten", |g| {
                let path = g.root.join("enshrouded.kfc_resources");
                let mut bytes = fs::read(&path).unwrap();
                bytes[0] = 8;
                rewrite_keeping_time(&path, &bytes)
            }),
            ("resources replaced", |g| {
                fs::write(g.root.join("enshrouded.kfc_resources"), b"vanilla").unwrap()
            }),
            ("backup replaced", |g| {
                fs::write(g.root.join("enshrouded.kfc.bak"), b"a newer original").unwrap()
            }),
            ("resource backup moved aside", |g| {
                fs::remove_file(g.root.join("enshrouded.kfc_resources.bak")).unwrap()
            }),
            ("game build", |g| {
                let manifest = g.root.join("../../appmanifest_1203620.acf");
                let text = fs::read_to_string(&manifest).unwrap().replace("26430625", "26500000");
                fs::write(manifest, text).unwrap()
            }),
            ("export rewritten", |g| {
                fs::write(g.exports.join("holster/map.json"), b"{\"v\":2}").unwrap()
            }),
            ("export removed", |g| {
                fs::remove_file(g.exports.join("holster/map.json")).unwrap()
            }),
            ("export added", |g| fs::write(g.exports.join("other.txt"), b"x").unwrap()),
            ("record of another scheme", |g| {
                let path = record_path(&g.root, "enshrouded");
                let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                value["scheme"] = json!("kiln-sf-prepared 0");
                fs::write(path, value.to_string()).unwrap()
            }),
        ];
        for (what, change) in changes {
            let g = game();
            let exports = g.exports.clone();
            record(&g.root, "enshrouded", &inputs("f1", Some(&exports))).unwrap();
            change(&g);
            assert!(
                current(&g.root, "enshrouded", &inputs("f1", Some(&exports))).is_err(),
                "{what}: must apply again"
            );
        }
    }

    #[test]
    fn other_pass_inputs_mean_apply() {
        let g = game();
        let exports = g.exports.clone();
        record(&g.root, "enshrouded", &inputs("f1", None)).unwrap();
        assert_eq!(current(&g.root, "enshrouded", &inputs("f1", None)), Ok(()));
        // another package plan or package file
        assert!(current(&g.root, "enshrouded", &inputs("f2", None)).is_err());
        // a pregame exporter now (the record has no export listing)
        assert!(current(&g.root, "enshrouded", &inputs("f1", Some(&exports))).is_err());
        // exports switched off
        let off = PassInputs {
            fingerprint: "f1",
            exports_enabled: false,
            exporters: None,
        };
        assert!(current(&g.root, "enshrouded", &off).is_err());
        // the server's record is its own
        assert!(current(&g.root, "enshrouded_server", &inputs("f1", None)).is_err());
    }

    #[test]
    fn exports_outside_a_pregame_exporter_pass_do_not_count() {
        let g = game();
        record(&g.root, "enshrouded", &inputs("f1", None)).unwrap();
        fs::write(g.exports.join("world-editor-capture.sfbp"), b"x").unwrap();
        assert_eq!(current(&g.root, "enshrouded", &inputs("f1", None)), Ok(()));
    }

    #[test]
    fn a_missing_steam_manifest_is_no_build_on_both_sides() {
        let g = game();
        fs::remove_file(g.root.join("../../appmanifest_1203620.acf")).unwrap();
        record(&g.root, "enshrouded", &inputs("f1", None)).unwrap();
        assert_eq!(current(&g.root, "enshrouded", &inputs("f1", None)), Ok(()));
    }
}
