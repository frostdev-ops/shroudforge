// SPDX-License-Identifier: MIT
//! Kiln layer: replace an outdated working baseline without the Updater.
//!
//! After a game update the working backups `<stem>.kfc.bak` and
//! `<stem>.kfc_resources.bak` belong to the previous game version. ShroudForge
//! has its Updater move them to the recycle bin and then captures new ones
//! from the updated game files. A runtime that a host bundles has no Updater:
//! the move failed, and with it every asset preparation after a game update.
//!
//! `set_aside` moves the old pair into ShroudForge's backups folder instead:
//! `<game>/shroudforge/backups/outdated-working-baseline/<stem>/<generation>/`.
//! Only the newest generation is kept, so the folder does not grow with every
//! game update.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

/// Folder below `<game>/shroudforge/backups` that receives the old pair.
pub const FOLDER: &str = "outdated-working-baseline";

/// The header backup goes last. Until then it still reads as outdated, so a
/// start after an interrupted move comes back here, and it names the
/// generation folder, so that start continues in the same folder. Moving it
/// first could leave an old companion beside a newly captured header backup,
/// and nothing checks a companion on its own.
/// The middle entries are kiln-eml's seal of the pair and the runtime's own record of it
/// (`record_working_baseline.rs`); both describe the old pair.
fn names(stem: &str) -> [String; 4] {
    [
        format!("{stem}.kfc_resources.bak"),
        format!("{stem}.kfc.bak.integrity"),
        format!("{stem}.kfc.baseline.json"),
        format!("{stem}.kfc.bak"),
    ]
}

// Never follow a link: these paths are in the game folder and in ShroudForge's data folder.
fn plain(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if meta.file_attributes() & 0x400 != 0 {
                    return Err(io::Error::other(format!("{} is a link", path.display())));
                }
            }
            if meta.file_type().is_symlink() {
                return Err(io::Error::other(format!("{} is a link", path.display())));
            }
            Ok(Some(meta))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn file(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match plain(path)? {
        Some(meta) if meta.is_file() => Ok(Some(meta)),
        Some(_) => Err(io::Error::other(format!(
            "{} is not a regular file",
            path.display()
        ))),
        None => Ok(None),
    }
}

fn folder(path: &Path) -> io::Result<()> {
    match plain(path)? {
        Some(meta) if !meta.is_dir() => Err(io::Error::other(format!(
            "{} is not a folder",
            path.display()
        ))),
        _ => Ok(()),
    }
}

/// `set_aside` for a game folder that has no ShroudForge installation: neither
/// the Updater nor an installed runtime library, which is what a host that
/// bundles the runtime leaves. `None` everywhere else: an installation keeps
/// ShroudForge's own way, the Updater and its recycle bin, also while its
/// Updater is missing for a moment during a self-update.
pub fn set_aside_without_updater(root: &Path, stem: &str) -> Option<Result<PathBuf, String>> {
    let installed = crate::paths::updater_executable(root).is_file()
        || crate::paths::runtime_library(root).is_file();
    (!installed).then(|| set_aside(root, stem))
}

/// Moves the working backups of `stem` (`enshrouded` or `enshrouded_server`)
/// out of the game folder and returns the folder that holds them now. Nothing
/// is moved when one of the files or folders involved is not what it should be.
pub fn set_aside(root: &Path, stem: &str) -> Result<PathBuf, String> {
    set_aside_with(root, stem, |from, to| fs::rename(from, to))
}

fn set_aside_with(
    root: &Path,
    stem: &str,
    mut move_file: impl FnMut(&Path, &Path) -> io::Result<()>,
) -> Result<PathBuf, String> {
    if !matches!(stem, "enshrouded" | "enshrouded_server") {
        return Err("unknown game target for the working asset backups".into());
    }
    (|| -> io::Result<PathBuf> {
        let names = names(stem);
        let mut found = Vec::new();
        let mut header = None;
        for name in &names {
            if let Some(meta) = file(&root.join(name))? {
                found.push(name);
                header = Some(meta);
            }
        }
        // `header` is the last one found; it must be the header backup itself.
        let header = header
            .filter(|_| found.last() == Some(&&names[3]))
            .ok_or_else(|| io::Error::other("there is no working header backup to set aside"))?;
        let generation = format!(
            "{}-{}",
            header.len(),
            header
                .modified()?
                .duration_since(UNIX_EPOCH)
                .map_or(0, |age| age.as_secs())
        );
        let mut kept = crate::paths::data_dir(root);
        for part in ["backups", FOLDER, stem, &generation] {
            folder(&kept)?;
            kept.push(part);
        }
        folder(&kept)?;
        fs::create_dir_all(&kept)?;
        let mut inside = Vec::new();
        for name in &names {
            if file(&kept.join(name))?.is_some() {
                inside.push(name);
            }
        }
        // The header backup arrives last, so a folder that holds one is a
        // finished generation: another pair with the same size and time. It is
        // replaced whole; its seal must not end up beside this pair. Without a
        // header backup the folder is this pair's own interrupted move.
        if inside.contains(&&names[3]) {
            for name in inside {
                fs::remove_file(kept.join(name))?;
            }
        }
        for name in found {
            move_file(&root.join(name), &kept.join(name))?;
        }
        // The pair is out of the game folder; what follows is housekeeping.
        if let Some(generations) = kept.parent().and_then(|parent| fs::read_dir(parent).ok()) {
            for entry in generations.flatten() {
                let older = entry.path();
                if older != kept && plain(&older).is_ok_and(|meta| meta.is_some_and(|meta| meta.is_dir())) {
                    let _ = fs::remove_dir_all(older);
                }
            }
        }
        Ok(kept)
    })()
    .map_err(|error| format!("could not set the outdated working asset backups aside: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("shroudforge-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        for (name, text) in files {
            fs::write(root.join(name), text).unwrap();
        }
        root
    }

    fn generations(root: &Path, stem: &str) -> Vec<PathBuf> {
        let mut folders: Vec<_> = fs::read_dir(root.join("shroudforge/backups").join(FOLDER).join(stem))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        folders.sort();
        folders
    }

    fn files(folder: &Path) -> Vec<(String, String)> {
        let mut files: Vec<_> = fs::read_dir(folder)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name().into_string().unwrap(),
                    fs::read_to_string(entry.path()).unwrap(),
                )
            })
            .collect();
        files.sort();
        files
    }

    fn list(files: &[(&str, &str)]) -> Vec<(String, String)> {
        files
            .iter()
            .map(|(name, text)| ((*name).to_owned(), (*text).to_owned()))
            .collect()
    }

    #[test]
    fn the_old_pair_its_seal_and_its_record_leave_the_game_folder_and_one_generation_is_kept() {
        let root = fixture(&[
            ("enshrouded.kfc", "live header"),
            ("enshrouded.kfc_resources", "live resources"),
            ("enshrouded.kfc.bak", "header 1"),
            ("enshrouded.kfc_resources.bak", "resources 1"),
            ("enshrouded.kfc.bak.integrity", "seal 1"),
            ("enshrouded.kfc.baseline.json", "record 1"),
            ("enshrouded_server.kfc.bak", "the other target"),
        ]);
        let first = set_aside(&root, "enshrouded").unwrap();
        assert_eq!(generations(&root, "enshrouded"), vec![first.clone()]);
        assert_eq!(
            files(&first),
            list(&[
                ("enshrouded.kfc.bak", "header 1"),
                ("enshrouded.kfc.bak.integrity", "seal 1"),
                ("enshrouded.kfc.baseline.json", "record 1"),
                ("enshrouded.kfc_resources.bak", "resources 1"),
            ])
        );
        for gone in [
            "enshrouded.kfc.bak",
            "enshrouded.kfc_resources.bak",
            "enshrouded.kfc.bak.integrity",
            "enshrouded.kfc.baseline.json",
        ] {
            assert!(!root.join(gone).exists(), "{gone} is still in the game folder");
        }
        assert_eq!(fs::read_to_string(root.join("enshrouded.kfc")).unwrap(), "live header");
        assert_eq!(
            fs::read_to_string(root.join("enshrouded_server.kfc.bak")).unwrap(),
            "the other target"
        );

        // The next game update: its pair replaces the kept one, the old seal included.
        fs::write(root.join("enshrouded.kfc.bak"), "second header").unwrap();
        fs::write(root.join("enshrouded.kfc_resources.bak"), "resources 2").unwrap();
        let second = set_aside(&root, "enshrouded").unwrap();
        assert_ne!(first, second);
        assert_eq!(generations(&root, "enshrouded"), vec![second.clone()]);
        assert_eq!(
            files(&second),
            list(&[
                ("enshrouded.kfc.bak", "second header"),
                ("enshrouded.kfc_resources.bak", "resources 2"),
            ])
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// Review (0.2.0-dev.31.13): a header backup without its companion (what an unfinished copy of an earlier runtime
    /// leaves, or another program) is set aside too, and never takes the place of a kept generation that holds a
    /// whole pair.
    #[test]
    fn a_header_backup_without_its_companion_never_removes_a_kept_whole_pair() {
        let root = fixture(&[
            ("enshrouded.kfc.bak", "header 1"),
            ("enshrouded.kfc_resources.bak", "resources 1"),
        ]);
        let whole = set_aside(&root, "enshrouded").unwrap();
        fs::write(root.join("enshrouded.kfc.bak"), "a header of its own").unwrap();
        let half = set_aside(&root, "enshrouded").unwrap();
        assert_ne!(whole, half);
        assert_eq!(generations(&root, "enshrouded"), { let mut both = vec![whole.clone(), half.clone()]; both.sort(); both });
        assert_eq!(
            files(&whole),
            list(&[("enshrouded.kfc.bak", "header 1"), ("enshrouded.kfc_resources.bak", "resources 1")])
        );
        // another half: it replaces the earlier half, and the whole pair stays (two generations at most)
        fs::write(root.join("enshrouded.kfc.bak"), "another header of its own, longer").unwrap();
        let other = set_aside(&root, "enshrouded").unwrap();
        assert_eq!(generations(&root, "enshrouded"), { let mut both = vec![whole.clone(), other.clone()]; both.sort(); both });
        // the next whole pair replaces them all, as before
        fs::write(root.join("enshrouded.kfc.bak"), "header 2").unwrap();
        fs::write(root.join("enshrouded.kfc_resources.bak"), "resources 2").unwrap();
        let second = set_aside(&root, "enshrouded").unwrap();
        assert_eq!(generations(&root, "enshrouded"), vec![second]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_interrupted_move_continues_in_the_same_folder() {
        let root = fixture(&[
            ("enshrouded_server.kfc.bak", "header"),
            ("enshrouded_server.kfc_resources.bak", "resources"),
            ("enshrouded_server.kfc.bak.integrity", "seal"),
        ]);
        // The move stops after each file in turn. The header backup must be
        // the one still in the game folder: it is what brings the next start
        // back here.
        for (moved_so_far, moves_this_time) in [(0, 0), (0, 1), (1, 1)] {
            let mut moves = 0;
            let error = set_aside_with(&root, "enshrouded_server", |from, to| {
                if moves == moves_this_time {
                    return Err(io::Error::other("the process ended here"));
                }
                moves += 1;
                fs::rename(from, to)
            })
            .unwrap_err();
            assert!(error.contains("the process ended here"), "{error}");
            assert_eq!(
                fs::read_to_string(root.join("enshrouded_server.kfc.bak")).unwrap(),
                "header",
                "after {} moves",
                moved_so_far + moves_this_time
            );
        }
        assert!(!root.join("enshrouded_server.kfc_resources.bak").exists());
        assert!(!root.join("enshrouded_server.kfc.bak.integrity").exists());
        let kept = generations(&root, "enshrouded_server").pop().unwrap();

        assert_eq!(set_aside(&root, "enshrouded_server").unwrap(), kept);
        assert_eq!(
            files(&kept),
            list(&[
                ("enshrouded_server.kfc.bak", "header"),
                ("enshrouded_server.kfc.bak.integrity", "seal"),
                ("enshrouded_server.kfc_resources.bak", "resources"),
            ])
        );
        assert!(!root.join("enshrouded_server.kfc.bak").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_finished_generation_of_the_same_name_is_replaced_not_mixed() {
        let root = fixture(&[
            ("enshrouded.kfc.bak", "header 1"),
            ("enshrouded.kfc_resources.bak", "resources 1"),
            ("enshrouded.kfc.bak.integrity", "seal 1"),
        ]);
        let time = fs::metadata(root.join("enshrouded.kfc.bak")).unwrap().modified().unwrap();
        let first = set_aside(&root, "enshrouded").unwrap();

        // Another pair whose header backup has the same size and time, and no seal.
        fs::write(root.join("enshrouded.kfc.bak"), "header 2").unwrap();
        fs::File::options()
            .write(true)
            .open(root.join("enshrouded.kfc.bak"))
            .unwrap()
            .set_modified(time)
            .unwrap();
        fs::write(root.join("enshrouded.kfc_resources.bak"), "resources 2").unwrap();
        assert_eq!(set_aside(&root, "enshrouded").unwrap(), first);
        assert_eq!(
            files(&first),
            list(&[
                ("enshrouded.kfc.bak", "header 2"),
                ("enshrouded.kfc_resources.bak", "resources 2"),
            ]),
            "the first pair's seal does not stay beside the second pair"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nothing_moves_when_a_backup_or_the_target_is_not_what_it_should_be() {
        let root = fixture(&[("enshrouded.kfc.bak", "header")]);
        assert!(set_aside(&root, "game").unwrap_err().contains("unknown game target"));
        assert!(set_aside(&root, "enshrouded_server").unwrap_err().contains("no working header backup"));

        // A companion without its header backup is not an outdated pair.
        fs::write(root.join("enshrouded_server.kfc_resources.bak"), "resources").unwrap();
        assert!(set_aside(&root, "enshrouded_server").unwrap_err().contains("no working header backup"));
        assert!(root.join("enshrouded_server.kfc_resources.bak").exists());

        fs::create_dir(root.join("enshrouded.kfc_resources.bak")).unwrap();
        assert!(set_aside(&root, "enshrouded").unwrap_err().contains("not a regular file"));
        assert_eq!(fs::read_to_string(root.join("enshrouded.kfc.bak")).unwrap(), "header");
        fs::remove_dir(root.join("enshrouded.kfc_resources.bak")).unwrap();

        // ShroudForge's backups path is occupied by a file.
        fs::create_dir(root.join("shroudforge")).unwrap();
        fs::write(root.join("shroudforge/backups"), "not a folder").unwrap();
        assert!(set_aside(&root, "enshrouded").unwrap_err().contains("not a folder"));
        assert_eq!(fs::read_to_string(root.join("enshrouded.kfc.bak")).unwrap(), "header");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_installed_shroudforge_keeps_its_updater_path() {
        for program in ["shroudforge-updater.exe", "shroudforge-runtime.dll"] {
            let root = fixture(&[("enshrouded.kfc.bak", "header")]);
            fs::create_dir(root.join("shroudforge")).unwrap();
            fs::write(root.join("shroudforge").join(program), "program").unwrap();
            assert!(set_aside_without_updater(&root, "enshrouded").is_none(), "{program}");
            assert!(root.join("enshrouded.kfc.bak").exists(), "{program}");
            fs::remove_dir_all(root).unwrap();
        }

        // What a host leaves: ShroudForge's data folder without its programs.
        let root = fixture(&[("enshrouded.kfc.bak", "header")]);
        fs::create_dir_all(root.join("shroudforge/config")).unwrap();
        let kept = set_aside_without_updater(&root, "enshrouded").unwrap().unwrap();
        assert_eq!(files(&kept), list(&[("enshrouded.kfc.bak", "header")]));
        assert!(!root.join("enshrouded.kfc.bak").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn links_are_not_followed() {
        let outside = fixture(&[("victim", "outside the game")]);
        let root = fixture(&[("enshrouded.kfc.bak", "header")]);
        std::os::unix::fs::symlink(outside.join("victim"), root.join("enshrouded.kfc_resources.bak")).unwrap();
        assert!(set_aside(&root, "enshrouded").unwrap_err().contains("is a link"));
        fs::remove_file(root.join("enshrouded.kfc_resources.bak")).unwrap();

        fs::create_dir(root.join("shroudforge")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("shroudforge/backups")).unwrap();
        assert!(set_aside(&root, "enshrouded").unwrap_err().contains("is a link"));
        assert_eq!(fs::read_to_string(root.join("enshrouded.kfc.bak")).unwrap(), "header");
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 1, "nothing was written through the link");
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }
}
