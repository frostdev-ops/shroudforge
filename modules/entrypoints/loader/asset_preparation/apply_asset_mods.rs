use super::format_report;
use crate::LoaderError;
use std::path::Path;

pub(super) fn run_inner(
    game_directory: &Path,
    file_name: &str,
    phase: &str,
) -> Result<(), LoaderError> {
    let loader_config = shroudforge_loader_config::config::read_loader(game_directory)
        .map_err(LoaderError::Pregame)?;
    let export_enabled = loader_config
        .pointer("/exports/enabled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let game_utf8 = game_directory.to_str().ok_or_else(|| {
        LoaderError::Environment(format!(
            "game directory is not valid UTF-8: {}",
            game_directory.display()
        ))
    })?;
    let environment = shroudforge_package::ModEnvironment::load(game_utf8)
        .map_err(|report| LoaderError::Environment(format_report(&report)))?;
    let (_, plan_issues) = environment.plan_report_detailed(
        file_name == "enshrouded_server",
        shroudforge_api::API_VERSION,
    );
    for issue in plan_issues {
        tracing::warn!(target: "shroudforge::startup", mod_id = %issue.mod_id,
            code = %issue.code, detail = %issue.detail, "Mod skipped during startup preparation");
    }
    let fingerprint = shroudforge_package::prepared::fingerprint(
        &environment,
        file_name == "enshrouded_server",
        shroudforge_api::API_VERSION,
    )
    .map_err(LoaderError::Pregame)?;
    // Kiln layer: whether the game's header is what the last pass left, read before that record goes: such game files
    // are never taken for a clean copy (verify_working_baseline.rs).
    let live_is_last_output =
        shroudforge_game_assets::prepared_stamp::left_live_header(game_directory, file_name);
    // Kiln layer: the record of the last pass goes before the files it describes change.
    shroudforge_game_assets::prepared_stamp::forget(game_directory, file_name)
        .map_err(LoaderError::Pregame)?;
    shroudforge_loader_config::config::write_document(
        game_directory,
        shroudforge_game_assets::documents::APPLIED,
        &serde_json::json!({
            "schemaVersion": 1, "status": "preparing", "target": file_name
        }),
    )
    .map_err(LoaderError::Pregame)?;
    // Kiln layer: one whole pass for an environment, so that a pass that failed in one package runs
    // once more without it (retry_without_failed_mod.rs); `failed` is the package a failure is pinned on.
    let server = file_name == "enshrouded_server";
    let failed = std::cell::RefCell::new(None);
    let apply = |environment: &shroudforge_package::ModEnvironment| -> Result<Vec<serde_json::Value>, LoaderError> {
        failed.replace(None);
        shroudforge_game_assets::transaction::recover(game_directory, file_name)
            .map_err(|error| LoaderError::Pregame(error.to_string()))?;
        let resources = game_directory.join(format!("{file_name}.kfc_resources"));
        // Kiln layer: the pair goes back only when it is whole and the source of the game's files; one that is not
        // their source is set aside and made anew from them (verify_working_baseline.rs). ShroudForge's own rule (a
        // backup of another game version) is one of those reasons.
        let mut recorded = true;
        match shroudforge_api::check_working_baseline(game_directory, file_name, live_is_last_output) {
            shroudforge_api::Baseline::Missing => {
                shroudforge_api::capture_working_baseline(game_directory, file_name)
                    .map_err(LoaderError::Pregame)?;
            }
            shroudforge_api::Baseline::Restore { recorded: whole } => {
                if !shroudforge_api::restore_checked(game_utf8, file_name) {
                    return Err(LoaderError::Pregame(format!(
                        "failed to restore the clean {file_name}.kfc baseline"
                    )));
                }
                recorded = whole;
            }
            shroudforge_api::Baseline::Retake(why) => {
                crate::recycle_incompatible_game_asset_backups(game_directory, file_name)
                    .map_err(LoaderError::Pregame)?;
                shroudforge_api::capture_working_baseline(game_directory, file_name)
                    .map_err(LoaderError::Pregame)?;
                let _ = shroudforge_logging::append(game_directory, 'W', "startup-assets", &format!(
                    "ShroudForge: its clean copy of the game data ({file_name}.kfc.bak, {file_name}.kfc_resources.bak) \
                     was not used: {why}. It was set aside and a new clean copy was made from the game's current files"
                ));
            }
            shroudforge_api::Baseline::Refuse(why) => {
                return Err(LoaderError::Pregame(format!("ShroudForge: {why}")));
            }
        }
        let resources = resources.to_str().ok_or_else(|| {
            LoaderError::Environment("companion resource file path is not valid UTF-8".into())
        })?;
        if !shroudforge_api::backup_companion_file(resources) {
            return Err(LoaderError::Pregame(format!(
                "failed to back up the clean {}.kfc_resources baseline",
                file_name
            )));
        }
        // Kiln layer: a whole pair without a record (from before records, or a header backup whose companion was only
        // made now) is recorded, so that later starts can tell it apart.
        if !recorded {
            shroudforge_api::record_working_baseline(game_directory, file_name)
                .map_err(LoaderError::Pregame)?;
        }
        if phase == "startup" {
            shroudforge_api::run_with_local_schema(
                environment,
                shroudforge_api::RunArgs {
                    file_name: file_name.into(),
                    options: shroudforge_api::RunOptions {
                        force_patch: true,
                        patch: true,
                        export: export_enabled,
                        export_dir: None,
                        phase: shroudforge_api::RuntimePhase::Pregame,
                        ..Default::default()
                    },
                },
            )
            .map_err(|error| {
                failed.replace(super::failed_mod(error.downcast_ref(), environment, server));
                LoaderError::Pregame(error.to_string())
            })?;
        } else {
            let api = super::parse_game_assets::parse_game_assets(game_directory, file_name)?;
            shroudforge_api::run(
                environment,
                api,
                shroudforge_api::RunArgs {
                    file_name: file_name.into(),
                    options: shroudforge_api::RunOptions {
                        skip_cache: true,
                        force_patch: true,
                        patch: true,
                        export: export_enabled,
                        export_dir: None,
                        ..Default::default()
                    },
                },
            )
            .map_err(|error| {
                failed.replace(super::failed_mod(error.downcast_ref(), environment, server));
                LoaderError::Pregame(error.to_string())
            })?;
        }
        let mods = environment
            .plan(
                file_name == "enshrouded_server",
                shroudforge_api::API_VERSION,
            )
            .into_iter()
            .filter(|item| item.info().requires_pregame())
            .map(|item| {
                Ok(serde_json::json!({"id": item.info().id, "version": item.info().version}))
            })
            .collect::<Result<Vec<_>, String>>()
            .map_err(LoaderError::Pregame)?;
        Ok(mods)
    };
    let (apply_result, retry) = match (apply(&environment), failed.take()) {
        (Err(error), Some(failed)) => {
            super::run_once_more(game_directory, &environment, server, failed, error, &apply)
        }
        (result, _) => (result, super::Retry::default()),
    };
    let mods = match apply_result {
        Ok(mods) => mods,
        Err(error) => {
            let mut record = serde_json::json!({
                "schemaVersion": 1, "status": "failed", "phase": phase, "target": file_name,
                "completedAt": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
                "error": error.to_string()
            });
            retry.record_into(&mut record);
            let _ = shroudforge_loader_config::config::write_document(
                game_directory,
                shroudforge_game_assets::documents::APPLIED,
                &record,
            );
            return Err(error);
        }
    };
    // Kiln layer: what this pass left, so that the next start can skip an unchanged pass. Without
    // the record the next start applies again, as before.
    let pregame_exporters = export_enabled
        && environment
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
            });
    let exports = shroudforge_loader_config::paths::export_dir(game_directory);
    if let Err(error) = shroudforge_game_assets::prepared_stamp::record(
        game_directory,
        file_name,
        &shroudforge_game_assets::prepared_stamp::PassInputs {
            fingerprint: &fingerprint,
            exports_enabled: export_enabled,
            exporters: pregame_exporters.then_some(exports.as_path()),
        },
    ) {
        tracing::warn!(target: "shroudforge::startup", target = file_name, %error,
            "Could not record the asset pass; the next start applies the asset mods again");
    }
    let mut record = serde_json::json!({
        "schemaVersion": 1, "status": "applied", "phase": phase, "target": file_name,
        "fingerprint": fingerprint,
        "completedAt": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
        "mods": mods
    });
    // Kiln layer: the package the pass ran without, and why
    retry.record_into(&mut record);
    shroudforge_loader_config::config::write_document(
        game_directory,
        shroudforge_game_assets::documents::APPLIED,
        &record,
    )
    .map_err(LoaderError::Pregame)?;
    Ok(())
}
