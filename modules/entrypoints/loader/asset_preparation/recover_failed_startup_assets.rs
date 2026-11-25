use super::{process_target_name, startup_asset_lease};
use crate::LoaderError;
use std::path::Path;

/// Kiln layer: what the recovery did with the game data.
enum Recovered {
    /// The working baseline went back over the game files (upstream's recovery).
    Restored,
    /// The game files were kept: they are a clean copy of their own, which the failed pass did not change, and the
    /// working baseline was taken from them (`Ok`), or could not be (`Err`: why; the next pass tries again).
    Kept { why: String, retaken: Result<(), String> },
}

pub(crate) fn recover_failed_startup(game_directory: &Path, detail: &str) -> bool {
    let result = (|| -> Result<Recovered, LoaderError> {
        let target = process_target_name(game_directory)?;
        let _lease = startup_asset_lease(&shroudforge_loader_config::paths::startup_asset_lock(
            game_directory,
        ))?;
        let mut record = serde_json::json!({
            "schemaVersion": 1, "status": "failed", "phase": "startup", "target": target,
            "completedAt": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
            "error": detail
        });
        // Kiln layer: what the failed pass recorded about its one retry (retry_without_failed_mod.rs)
        // stays with its error.
        if let Ok(previous) = shroudforge_loader_config::config::read_document(
            game_directory,
            shroudforge_game_assets::documents::APPLIED,
        ) && previous["status"] == "failed"
            && previous["error"].as_str() == Some(detail)
        {
            for key in ["skipped", "retryError"] {
                if let Some(value) = previous.get(key) {
                    record[key] = value.clone();
                }
            }
        }
        shroudforge_loader_config::config::write_document(
            game_directory,
            shroudforge_game_assets::documents::APPLIED,
            &record,
        )
        .map_err(LoaderError::Pregame)?;
        shroudforge_game_assets::transaction::recover(game_directory, target)
            .map_err(|error| LoaderError::Pregame(error.to_string()))?;
        // Kiln layer: the pass's own check decides (mod-runtime/startup/verify_working_baseline.rs). Upstream's
        // recovery only restored the pair: a pair of the previous game build (a game update) cannot go back over
        // the game files, so after a pass that failed before it reached the working baseline (kiln-win1, 2026-11-25:
        // the loader settings) the recovery failed too and no ShroudForge mod ran, with no word of what to do.
        // Game files that the check takes for a clean copy of their own are the files that pass left untouched, so the
        // game starts on them, and the pair is made from them as the pass would have; a refusal says what to do.
        let live_is_last_output =
            shroudforge_game_assets::prepared_stamp::left_live_header(game_directory, target);
        let game_utf8 = game_directory
            .to_str()
            .ok_or_else(|| LoaderError::Environment("game directory is not valid UTF-8".into()))?;
        match shroudforge_api::check_working_baseline(game_directory, target, live_is_last_output) {
            shroudforge_api::Baseline::Restore { .. } => {
                if !shroudforge_api::restore_checked(game_utf8, target) {
                    return Err(LoaderError::Pregame(
                        "could not restore both baseline asset files".into(),
                    ));
                }
                Ok(Recovered::Restored)
            }
            shroudforge_api::Baseline::Refuse(why) => {
                Err(LoaderError::Pregame(format!("ShroudForge: {why}")))
            }
            shroudforge_api::Baseline::Missing => Ok(Recovered::Kept {
                why: format!("there was no clean copy ({target}.kfc.bak) yet"),
                retaken: shroudforge_api::capture_working_baseline(game_directory, target),
            }),
            shroudforge_api::Baseline::Retake(why) => Ok(Recovered::Kept {
                retaken: crate::recycle_incompatible_game_asset_backups(game_directory, target)
                    .and_then(|()| shroudforge_api::capture_working_baseline(game_directory, target)),
                why: format!("the clean copy ({target}.kfc.bak, {target}.kfc_resources.bak) was not used: {why}"),
            }),
        }
    })();
    match result {
        Ok(Recovered::Restored) => {
            let _ = shroudforge_logging::append(
                game_directory,
                'W',
                "startup-assets",
                "Asset mods failed; saved baseline restored. Continuing game startup without asset patches.",
            );
            true
        }
        Ok(Recovered::Kept { why, retaken }) => {
            let taken = match retaken {
                Ok(()) => "the game's own files were kept as the new clean copy (the old one was set aside)".to_string(),
                Err(error) => format!(
                    "the game's own files were kept; a new clean copy could not be made from them ({error}), the next \
                     start tries again"
                ),
            };
            let _ = shroudforge_logging::append(
                game_directory,
                'W',
                "startup-assets",
                &format!(
                    "Asset mods failed; {why}. The failed pass changed no game file, so {taken}. Continuing game \
                     startup without asset patches."
                ),
            );
            true
        }
        Err(error) => {
            // Kiln layer: the record says why the game data could not be put back, and what the player does, beside the
            // pass's own error (Kiln's launcher shows both: kiln-manage sf_startup.rs).
            if let Ok(mut record) = shroudforge_loader_config::config::read_document(
                game_directory,
                shroudforge_game_assets::documents::APPLIED,
            ) && record["status"] == "failed"
                && record["error"].as_str() == Some(detail)
            {
                record["recoveryError"] = error.to_string().into();
                let _ = shroudforge_loader_config::config::write_document(
                    game_directory,
                    shroudforge_game_assets::documents::APPLIED,
                    &record,
                );
            }
            let _ = shroudforge_logging::append(
                game_directory,
                'E',
                "startup-assets",
                &format!("Asset baseline recovery failed: {error}"),
            );
            false
        }
    }
}
