use super::{process_target_name, startup_asset_lease};
use crate::LoaderError;
use std::path::Path;

pub(crate) fn recover_failed_startup(game_directory: &Path, detail: &str) -> bool {
    let result = (|| -> Result<(), LoaderError> {
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
        for extension in ["kfc.bak", "kfc_resources.bak"] {
            if !game_directory
                .join(format!("{target}.{extension}"))
                .is_file()
            {
                return Err(LoaderError::Pregame(format!(
                    "missing {target}.{extension}; cannot restore asset baseline"
                )));
            }
        }
        let game_utf8 = game_directory
            .to_str()
            .ok_or_else(|| LoaderError::Environment("game directory is not valid UTF-8".into()))?;
        if !shroudforge_api::restore(game_utf8, target) {
            return Err(LoaderError::Pregame(
                "could not restore both baseline asset files".into(),
            ));
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            let _ = shroudforge_logging::append(
                game_directory,
                'W',
                "startup-assets",
                "Asset mods failed; saved baseline restored. Continuing game startup without asset patches.",
            );
            true
        }
        Err(error) => {
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
