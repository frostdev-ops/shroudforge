use crate::config::{installation_lock, write_json};
use crate::read_loader_state::read_state_file;
use serde_json::{Value, json};
use std::path::Path;

pub fn update_state_section(
    root: &Path,
    section: &str,
    update: impl FnOnce(Option<&Value>) -> Result<Value, String>,
) -> Result<(), String> {
    let _lock = installation_lock(root)?;
    let mut state = read_state_file(root)?;
    if state
        .get("schemaVersion")
        .and_then(Value::as_u64)
        .is_some_and(|version| version != 1)
    {
        return Err(format!(
            "unsupported schemaVersion in {}",
            crate::paths::state_file(root).display()
        ));
    }
    let value = update(state.get(section))?;
    // Kiln layer: a section that already holds this value is not written again. Every write is a
    // flushed temp file plus a rename of the whole state file, so an unchanged status publication
    // would only cost disk time (dev29 inbox 50 item 7). A state file read here already has
    // schemaVersion 1 (state.schema.json requires it), so nothing else would change.
    if state.get(section) == Some(&value) {
        return Ok(());
    }
    state["schemaVersion"] = json!(1);
    state[section] = value;
    write_json(&crate::paths::state_file(root), &state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modified(root: &Path) -> std::time::SystemTime {
        std::fs::metadata(crate::paths::state_file(root))
            .unwrap()
            .modified()
            .unwrap()
    }

    #[test]
    fn an_unchanged_section_is_not_written_again() {
        let root = tempfile::tempdir().unwrap();
        update_state_section(root.path(), "runtime", |_| Ok(json!({"running": true, "pid": 7})))
            .unwrap();
        let path = crate::paths::state_file(root.path());
        // Mark the file so a rewrite shows even on a coarse clock.
        let before = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(before)
            .unwrap();
        update_state_section(root.path(), "runtime", |_| Ok(json!({"pid": 7, "running": true})))
            .unwrap();
        assert_eq!(modified(root.path()), before, "an equal value (any key order) is skipped");

        update_state_section(root.path(), "runtime", |_| Ok(json!({"running": false, "pid": 7})))
            .unwrap();
        assert_ne!(modified(root.path()), before, "a changed value is written");
        let state: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(state["runtime"]["running"], false);
    }
}
