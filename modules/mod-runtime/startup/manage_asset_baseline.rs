use super::*;
use kfc::container::KFCFile;

/// Kiln layer: a backup gets its name only when its copy is complete. Restore
/// takes any file under the backup's name for a whole one, and the companion
/// is some 200 MiB: a start that ended during a direct copy left a part of it
/// under that name, and a later start copied that part over the game's file.
/// An unfinished copy now stays under `<backup>.partial`, which nothing reads.
fn copy_complete(
    source: impl AsRef<std::path::Path>,
    backup: impl AsRef<std::path::Path>,
) -> std::io::Result<()> {
    let backup = backup.as_ref();
    let partial = partial_path(backup);
    copy_flushed(source, &partial)?;
    std::fs::rename(&partial, backup)
}

/// Kiln layer: where the copy for `backup` is made before it gets that name.
fn partial_path(backup: &std::path::Path) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("{}.partial", backup.display()))
}

/// Kiln layer: `source` copied to `partial` and flushed to the disk.
fn copy_flushed(source: impl AsRef<std::path::Path>, partial: &std::path::Path) -> std::io::Result<()> {
    std::fs::copy(source, partial)?;
    std::fs::OpenOptions::new()
        .write(true)
        .open(partial)?
        .sync_all()
}

pub fn create_backup(kfc_path: impl AsRef<Path>) -> Result<PathBuf, ()> {
    let path = kfc_path.as_ref();
    let mut bak_path = path.to_string();
    bak_path.push_str(".bak");
    let bak_path = Path::new(&bak_path);

    if bak_path.exists() && !validate_backup(path, bak_path) {
        debug!(
            path = ?path,
            backup_path = ?bak_path,
            "Backup file is not compatible with the current KFC file, creating a new backup.",
        );

        if !bak_path.is_file() {
            error!(
                path = ?bak_path,
                "Backup path is not a file, please remove it manually.",
            );

            return Err(());
        }

        if let Err(e) = std::fs::remove_file(bak_path) {
            warn!(
                path = ?bak_path,
                error = %e,
                "Failed to remove old backup file",
            );
        }
    }

    if !bak_path.exists() {
        if let Err(e) = copy_complete(path, bak_path) {
            error!(
                path = ?bak_path,
                error = %e,
                "Failed to create backup file",
            );

            return Err(());
        } else {
            info!(
                path = ?bak_path,
                "Backup file created successfully",
            );
        }
    }

    Ok(bak_path.to_path_buf())
}

pub fn restore_backup(kfc_path: impl AsRef<Path>) -> Result<(), ()> {
    let path = kfc_path.as_ref();
    let mut bak_path = path.to_string();
    bak_path.push_str(".bak");
    let bak_path = Path::new(&bak_path);

    if !bak_path.exists() {
        warn!(
            path = ?bak_path,
            "Backup file does not exist, cannot restore.",
        );
        return Err(());
    }

    if !bak_path.is_file() {
        error!(
            path = ?bak_path,
            "Backup path is not a file, please remove it manually.",
        );
        return Err(());
    }

    if !validate_backup(path, bak_path) {
        warn!(
            path = ?path,
            backup_path = ?bak_path,
            "Backup file is not compatible with the current KFC file, cannot restore.",
        );
        return Err(());
    }

    if let Err(e) = std::fs::copy(bak_path, path) {
        error!(
            path = ?path,
            backup_path = ?bak_path,
            error = %e,
            "Failed to restore backup file",
        );
        return Err(());
    } else {
        info!(
            path = ?path,
            backup_path = ?bak_path,
            "Backup file restored successfully",
        );
    }

    Ok(())
}

/// Keeps the original companion resource file before pregame asset mods run.
pub fn create_companion_backup(
    path: impl AsRef<std::path::Path>,
) -> Result<std::path::PathBuf, ()> {
    let path = path.as_ref();
    let backup = std::path::PathBuf::from(format!("{}.bak", path.display()));
    if backup.exists() {
        if backup.is_file() {
            return Ok(backup);
        }
        error!(path = ?backup, "Companion backup path is not a file");
        return Err(());
    }
    copy_complete(path, &backup).map_err(|error| {
        error!(path = ?path, backup_path = ?backup, error = %error, "Failed to create companion backup");
    })?;
    info!(path = ?path, backup_path = ?backup, "Companion backup created");
    Ok(backup)
}

/// Restores a companion file when ShroudForge has a saved original.
pub fn restore_companion_backup(path: impl AsRef<std::path::Path>) -> Result<bool, ()> {
    let path = path.as_ref();
    let backup = std::path::PathBuf::from(format!("{}.bak", path.display()));
    if !backup.exists() {
        return Ok(false);
    }
    if !backup.is_file() {
        error!(path = ?backup, "Companion backup path is not a file");
        return Err(());
    }
    std::fs::copy(&backup, path).map_err(|error| {
        error!(path = ?path, backup_path = ?backup, error = %error, "Failed to restore companion backup");
    })?;
    info!(path = ?path, backup_path = ?backup, "Companion backup restored");
    Ok(true)
}

#[cfg(test)]
mod companion_backup_tests {
    use super::{create_companion_backup, restore_companion_backup};

    #[test]
    fn companion_resource_backup_restores_original_bytes() {
        let root = std::env::temp_dir().join(format!(
            "shroudforge-resource-backup-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let resource = root.join("enshrouded_server.kfc_resources");
        std::fs::write(&resource, b"original resources").unwrap();

        create_companion_backup(&resource).unwrap();
        std::fs::write(&resource, b"modded resources").unwrap();

        assert!(restore_companion_backup(&resource).unwrap());
        assert_eq!(std::fs::read(&resource).unwrap(), b"original resources");
        std::fs::remove_dir_all(root).unwrap();
    }

    // Kiln layer.
    #[test]
    fn an_unfinished_backup_copy_is_never_restored() {
        let root = std::env::temp_dir().join(format!(
            "shroudforge-resource-backup-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let resource = root.join("enshrouded_server.kfc_resources");
        let backup = root.join("enshrouded_server.kfc_resources.bak");
        let partial = root.join("enshrouded_server.kfc_resources.bak.partial");
        std::fs::write(&resource, b"original resources").unwrap();
        // What a start leaves that ended during the copy.
        std::fs::write(&partial, b"origin").unwrap();

        assert!(!restore_companion_backup(&resource).unwrap(), "there is no backup yet");
        assert_eq!(std::fs::read(&resource).unwrap(), b"original resources");

        create_companion_backup(&resource).unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), b"original resources");
        assert!(!partial.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}

pub fn validate_backup(kfc_path: &Path, kfc_path_bak: &Path) -> bool {
    let file_bak = match KFCFile::from_path(kfc_path_bak, true) {
        Ok(file) => file,
        Err(_) => return false,
    };

    let file = match KFCFile::from_path(kfc_path, true) {
        Ok(file) => file,
        Err(_) => return false,
    };

    file.game_version() == file_bak.game_version() && same_kfc_format(kfc_path, kfc_path_bak)
}

pub fn is_valid_kfc_file(path: &Path) -> bool {
    KFCFile::from_path(path, true).is_ok()
}

pub fn working_baseline_is_incompatible(game_dir: impl AsRef<Path>, file_name: &str) -> bool {
    let kfc = game_dir.as_ref().join(format!("{file_name}.kfc"));
    let backup = game_dir.as_ref().join(format!("{file_name}.kfc.bak"));
    backup.is_file() && is_valid_kfc_file(&kfc) && !validate_backup(&kfc, &backup)
}

pub fn create_working_kfc_backup(
    game_dir: impl AsRef<Path>,
    file_name: &str,
) -> Result<PathBuf, ()> {
    create_backup(game_dir.as_ref().join(format!("{file_name}.kfc")))
}

/// ShroudForge compares the on-disk markers here instead of depending on
/// `KFCFile::format()`, which is not part of upstream `main`'s API. This only
/// changes backup validation. KFC parsing and EML startup remain unchanged.
fn same_kfc_format(kfc_path: &Path, backup_path: &Path) -> bool {
    fn read_format(path: &Path) -> Option<u8> {
        let mut file = std::fs::File::open(path).ok()?;
        let mut header = [0; 4];
        std::io::Read::read_exact(&mut file, &mut header).ok()?;
        if &header[..3] != b"KFC" {
            return None;
        }
        matches!(header[3], b'3' | b'4').then_some(header[3])
    }

    matches!(
        (read_format(kfc_path), read_format(backup_path)),
        (Some(current), Some(backup)) if current == backup
    )
}

pub fn restore(game_dir: impl AsRef<Path>, file_name: &str) -> bool {
    // Kiln layer: only a whole pair that the game files are, or were built from, goes back
    // (verify_working_baseline.rs).
    use super::verify_working_baseline::{Baseline, check_working_baseline};
    match check_working_baseline(game_dir.as_ref().as_std_path(), file_name, false) {
        Baseline::Restore { .. } | Baseline::Missing => {}
        Baseline::Retake(why) | Baseline::Refuse(why) => {
            warn!(
                stem = file_name,
                reason = %why,
                "The working asset backups were not restored over the game files",
            );
            return false;
        }
    }
    restore_checked(game_dir, file_name)
}

/// Kiln layer: `restore` without the check, for a caller that has just made it.
pub fn restore_checked(game_dir: impl AsRef<Path>, file_name: &str) -> bool {
    let game = game_dir.as_ref().as_std_path();
    let result = (|| -> std::io::Result<()> {
        let stage = shroudforge_game_assets::transaction::begin(game, file_name)?;
        // Copy and flush the whole pair before publishing either file. The existing transaction publishes the header
        // last and retains rollback copies if a replacement fails or the process ends during publication.
        for extension in ["kfc", "kfc_resources"] {
            let name = format!("{file_name}.{extension}");
            std::fs::copy(game.join(format!("{name}.bak")), stage.join(name))?;
        }
        shroudforge_game_assets::transaction::commit(game, file_name)
    })();
    if let Err(error) = result {
        error!(stem = file_name, error = %error, "Failed to restore the complete asset baseline");
        if let Err(error) = shroudforge_game_assets::transaction::abort(game, file_name) {
            error!(stem = file_name, error = %error, "Baseline rollback remains pending for the next start");
        }
        return false;
    }
    true
}

pub fn backup_companion_file(path: impl AsRef<std::path::Path>) -> bool {
    crate::startup::manage_asset_baseline::create_companion_backup(path).is_ok()
}

/// Kiln layer: a new working baseline from the game files, both backups at once (a companion backup that is
/// there without its header backup is replaced too: it belongs to no pair), and its record.
///
/// Both copies are made and flushed before either gets its name, and the header backup gets its name last. A copy
/// that cannot be finished (a full disk: the resource file is some 215 MB) leaves no backup and removes its `.partial`
/// files; a start that ends between the two renames leaves a companion without its header backup, which the next start
/// replaces (there is no pair yet). A header backup without its companion is never left: the next start would set it
/// aside as an outdated pair, in the place of the old pair kept aside (review, 0.2.0-dev.31.13).
pub fn capture_working_baseline(game_dir: impl AsRef<std::path::Path>, file_name: &str) -> Result<(), String> {
    let game = game_dir.as_ref();
    let pair = ["kfc_resources", "kfc"].map(|extension| {
        let source = game.join(format!("{file_name}.{extension}"));
        let backup = game.join(format!("{file_name}.{extension}.bak"));
        let partial = partial_path(&backup);
        (source, backup, partial)
    });
    for (source, backup, partial) in &pair {
        if let Err(error) = copy_flushed(source, partial) {
            for (_, _, partial) in &pair {
                let _ = std::fs::remove_file(partial);
            }
            return Err(format!("could not copy {} to {}: {error}", source.display(), backup.display()));
        }
    }
    for (_, backup, partial) in &pair {
        std::fs::rename(partial, backup)
            .map_err(|error| format!("could not name {} {}: {error}", partial.display(), backup.display()))?;
    }
    shroudforge_game_assets::baseline_record::record(game, file_name)
}

/// Kiln layer: records the working baseline as it is now (a whole pair that had no record).
pub fn record_working_baseline(game_dir: impl AsRef<std::path::Path>, file_name: &str) -> Result<(), String> {
    shroudforge_game_assets::baseline_record::record(game_dir.as_ref(), file_name)
}
