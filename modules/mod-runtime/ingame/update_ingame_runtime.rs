use super::IngameRuntime;
use crate::env::AppState;

/// Kiln layer: how often the running runtime republishes its status (state.json "runtime"). Every
/// publication is a flushed rewrite of the whole state file, so once a second cost the disk the most
/// of anything ShroudForge does while playing (dev29 inbox 50 item 7). The readers take the status
/// as current while `updatedAt` (whole seconds) is at most 5 s old: the modloader UI
/// (`modloader/ui/host/read_configuration_status.rs`) and the player-name lookup
/// (`player-name/lib.rs`); Kiln's launcher allows 30 s (`kiln-manage/src/shroudforge.rs`). 3 s plus
/// the rounding of `updatedAt` and a late frame stays inside the 5 s.
pub(super) const STATUS_PUBLICATION_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(3);
/// The tightest reader's limit above, for the test below.
#[cfg(test)]
const STRICTEST_READER_STALENESS_SECONDS: u64 = 5;

impl IngameRuntime {
    pub fn update(&mut self, delta_seconds: f64) {
        let mut diagnostic_host = super::run_diagnostic_job::RuntimeDiagnosticHost::new(
            &self.runner,
            self.active_mod_ids(),
        );
        self.diagnostics.tick_with_host(&mut diagnostic_host);
        self.runner
            .lua
            .app_data_ref::<AppState>()
            .unwrap()
            .poll_native_dll_loads();
        self.process_runtime_reload_request();
        if std::time::Instant::now() >= self.next_status {
            let config = shroudforge_loader_config::config::read_loader(&self.root).ok();
            if config
                .as_ref()
                .and_then(|value| value.pointer("/modules/network/enabled"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                let (authorized_clients, restrict_network_clients) = config
                    .as_ref()
                    .map(shroudforge_steam_networking::read_network_allowlist)
                    .unwrap_or_else(|| (Vec::new(), true));
                shroudforge_steam_networking::report_network_health(
                    &authorized_clients,
                    restrict_network_clients,
                    || {
                        self.runner
                            .lua
                            .app_data_ref::<AppState>()
                            .unwrap()
                            .is_server()
                    },
                );
            } else {
                shroudforge_steam_networking::service_shutdown();
            }
            self.refresh_configuration();
            let now = std::time::Instant::now();
            if now >= self.next_status_publication {
                self.publish_status(true);
                self.next_status_publication = now + STATUS_PUBLICATION_INTERVAL;
            }
            self.next_status = now + std::time::Duration::from_secs(1);
        }
        crate::env::shroudforge::dispatch_ui_actions(&self.runner.lua, &self.active_mod_ids());
        self.update_mod_callbacks(delta_seconds);
        let state = self.runner.lua.app_data_ref::<AppState>().unwrap();
        state.advance_loaded_resource_indexes();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn status_publication_stays_fresh_for_the_strictest_reader() {
        // One second for the whole-second rounding of `updatedAt`, one for a late frame.
        assert!(
            super::STATUS_PUBLICATION_INTERVAL.as_secs() + 2
                <= super::STRICTEST_READER_STALENESS_SECONDS
        );
        assert!(super::STATUS_PUBLICATION_INTERVAL.as_secs() >= 3);
    }
}
