use super::{
    already_applied, ensure_original_gamefiles, export_pass_needed, format_report,
    process_target_name, run_inner, startup_asset_lease,
};
use crate::LoaderError;
use std::path::Path;

pub(crate) fn run_startup(game_directory: impl AsRef<Path>) -> Result<(), LoaderError> {
    let game_directory = game_directory.as_ref();
    let file_name = process_target_name(game_directory)?;
    shroudforge_loader_config::config::initialize_loader_config(game_directory)
        .map_err(LoaderError::Pregame)?;
    // Capture a first-install original before the early return for installations
    // that do not have asset mods. Uncertain legacy installs remain unclassified.
    ensure_original_gamefiles(game_directory, file_name);
    let lock_path = shroudforge_loader_config::paths::startup_asset_lock(game_directory);
    let _lease = startup_asset_lease(&lock_path)?;
    let export_pass_needed = export_pass_needed(game_directory, file_name)?;
    if already_applied(game_directory, file_name, export_pass_needed)? {
        // Enshrouded will load the already-prepared KFC data during startup.
        tracing::debug!(
            "Startup KFC assets are already prepared for this game and mod configuration; no rewrite is needed"
        );
        return Ok(());
    }

    let game_utf8 = game_directory.to_str().ok_or_else(|| {
        LoaderError::Environment(format!(
            "game directory is not valid UTF-8: {}",
            game_directory.display()
        ))
    })?;
    let environment = shroudforge_package::ModEnvironment::load(game_utf8)
        .map_err(|report| LoaderError::Environment(format_report(&report)))?;
    let (startup_plan, plan_issues) = environment.plan_report_detailed(
        file_name == "enshrouded_server",
        shroudforge_api::API_VERSION,
    );
    let has_startup_mods = startup_plan
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
        for issue in plan_issues {
            tracing::warn!(target: "shroudforge::startup", mod_id = %issue.mod_id,
                code = %issue.code, detail = %issue.detail, "Startup mod skipped by dependency or compatibility planning");
        }
        return Ok(());
    }

    tracing::debug!(target: "shroudforge::startup", "Applying asset mods during early process startup");
    run_inner(game_directory, file_name, "startup")
}
