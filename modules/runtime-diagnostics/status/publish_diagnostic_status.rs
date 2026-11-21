use crate::{Session, seconds};
use serde_json::json;
use std::time::{Duration, Instant};

/// Kiln layer: an unchanged status is written again after this long only, so that `updatedAt` stays
/// inside the 65 s that `status()` (read_diagnostic_status.rs) allows before it reports
/// "process-not-reporting". Everything else in the status changes only with a job, a setting or a
/// sample, and is written when it does (dev29 inbox 50 item 7: this ran every 2 s, diagnostics on
/// or off, and each run was a flushed rewrite of the whole state file).
pub(crate) const UNCHANGED_STATUS_REFRESH: Duration = Duration::from_secs(30);

impl Session {
    pub(super) fn publish(&mut self) {
        let mut value = json!({"schemaVersion":1,"pid":std::process::id(),"updatedAt":seconds(),"active":self.active,
            "jobId":self.job.as_ref().map(|job| job.id.as_str()),
            "reason":self.reason,"remainingSeconds":if self.active {self.limit().saturating_sub(self.started.elapsed().as_secs())}else{0},
            "lastSampleAt":self.last_sample,"report":self.last_report.clone()});
        let updated_at = value["updatedAt"].take();
        let now = Instant::now();
        if self.published.as_ref().is_some_and(|(content, at)| {
            *content == value && now.duration_since(*at) < UNCHANGED_STATUS_REFRESH
        }) {
            return;
        }
        let content = value.clone();
        value["updatedAt"] = updated_at;
        // Off the mods' loop in game: a write could stall every mod at times.
        if self.background_io {
            // Kiln layer: a failed background write is only logged; the status is written again with
            // the next change, or after UNCHANGED_STATUS_REFRESH, well inside status()'s 65 s.
            shroudforge_loader_config::config::write_document_in_background(
                &self.root,
                crate::documents::STATUS,
                value,
                |error| tracing::error!(target:"shroudforge::diagnostics","Status publication failed: {error}"),
            );
            self.published = Some((content, now));
        } else {
            match shroudforge_loader_config::config::write_document(
                &self.root,
                crate::documents::STATUS,
                &value,
            ) {
                Ok(()) => self.published = Some((content, now)),
                Err(error) => {
                    // Not recorded as published: the next tick tries again.
                    self.published = None;
                    tracing::error!(target:"shroudforge::diagnostics","Status publication failed: {error}");
                }
            }
        }
    }
}
