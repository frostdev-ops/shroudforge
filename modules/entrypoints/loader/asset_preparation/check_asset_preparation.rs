use super::format_report;
use crate::LoaderError;
use std::path::Path;

pub(super) fn export_pass_needed(
    game_directory: &Path,
    file_name: &str,
) -> Result<bool, LoaderError> {
    let config = shroudforge_loader_config::config::read_loader(game_directory)
        .map_err(LoaderError::Pregame)?;
    if !config
        .pointer("/exports/enabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(false);
    }
    let root = game_directory.to_str().ok_or_else(|| {
        LoaderError::Environment(format!(
            "game directory is not valid UTF-8: {}",
            game_directory.display()
        ))
    })?;
    let environment = shroudforge_package::ModEnvironment::load(root)
        .map_err(|report| LoaderError::Environment(format_report(&report)))?;
    Ok(environment
        .plan(
            file_name == "enshrouded_server",
            shroudforge_api::API_VERSION,
        )
        .iter()
        .any(|item| {
            item.info().requires_pregame()
                && item
                    .info()
                    .capabilities
                    .contains(&shroudforge_package::Capability::Export)
        }))
}

/// Kiln layer: `export_pass_needed` no longer forces a pass by itself. A match also needs the
/// record of the last pass (`shroudforge_game_assets::prepared_stamp`): the game files it left, and
/// while exports are on what the pass used in the export folder, unchanged since.
pub(super) fn already_applied(
    game_directory: &Path,
    file_name: &str,
    export_pass_needed: bool,
) -> Result<bool, LoaderError> {
    let game_utf8 = game_directory.to_str().ok_or_else(|| {
        LoaderError::Environment(format!(
            "game directory is not valid UTF-8: {}",
            game_directory.display()
        ))
    })?;
    let environment = shroudforge_package::ModEnvironment::load(game_utf8)
        .map_err(|report| LoaderError::Environment(format_report(&report)))?;
    let server = file_name == "enshrouded_server";
    let has_startup_mods = environment
        .plan(server, shroudforge_api::API_VERSION)
        .iter()
        .any(|item| item.info().requires_pregame());
    let has_previous_apply = shroudforge_loader_config::config::read_document(
        game_directory,
        shroudforge_game_assets::documents::APPLIED,
    )
    .ok()
    .is_some_and(|value| {
        matches!(
            value["status"].as_str(),
            Some("applied" | "preparing" | "failed")
        )
    });
    if !has_startup_mods && !has_previous_apply {
        return Ok(true);
    }
    let fingerprint = shroudforge_package::prepared::fingerprint(
        &environment,
        server,
        shroudforge_api::API_VERSION,
    )
    .map_err(LoaderError::Pregame)?;
    if !shroudforge_package::prepared::matches(game_directory, &fingerprint) {
        return Ok(false);
    }
    let exports_enabled = shroudforge_loader_config::config::read_loader(game_directory)
        .map_err(LoaderError::Pregame)?
        .pointer("/exports/enabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let exports = shroudforge_loader_config::paths::export_dir(game_directory);
    // While exports are on, any pregame package may have used the export API (it checks the switch,
    // not the `export` capability), so the record's export part is compared then, as the pass
    // records it (apply_asset_mods.rs). `export_pass_needed` implies exports on.
    let inputs = shroudforge_game_assets::prepared_stamp::PassInputs {
        fingerprint: &fingerprint,
        exports_enabled,
        exporters: (exports_enabled || export_pass_needed).then_some(exports.as_path()),
    };
    match shroudforge_game_assets::prepared_stamp::current(game_directory, file_name, &inputs) {
        Ok(()) => Ok(true),
        Err(reason) => {
            tracing::info!(target: "shroudforge::startup", target = file_name, %reason,
                "Startup asset mods are applied again");
            Ok(false)
        }
    }
}
