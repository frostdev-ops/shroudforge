use super::*;
use serde_json::json;
use std::time::{Duration, Instant};
#[test]
fn disabled_module_does_not_measure_and_enabled_snapshot_finishes() {
    let root = tempfile::tempdir().unwrap();
    shroudforge_loader_config::config::update_loader(root.path(), |value| {
        value["modules"]["runtimeDiagnostics"]["enabled"] = json!(false);
        Ok(())
    })
    .unwrap();
    let mut session = Session::new(root.path());
    session.measure(
        "mods",
        "example",
        "on_update",
        Duration::from_millis(30),
        false,
    );
    assert!(session.measurements.is_empty());
    assert!(request(root.path(), "snapshot").is_err());
    shroudforge_loader_config::config::update_loader(root.path(), |value| {
        value["modules"]["runtimeDiagnostics"]["enabled"] = json!(true);
        Ok(())
    })
    .unwrap();
    request(root.path(), "snapshot").unwrap();
    session.next_config = Instant::now();
    session.tick();
    assert!(!session.active);
    assert_eq!(session.reason, "snapshot-complete");
    assert!(session.last_sample > 0);
    assert_eq!(
        shroudforge_loader_config::config::read_loader(root.path()).unwrap()["modules"]["runtimeDiagnostics"]
            ["sessionActive"],
        false
    );
    assert_eq!(status(root.path())["reason"], "snapshot-complete");
}
#[test]
fn deadline_is_independent_of_sample_interval_and_restart_works() {
    let root = tempfile::tempdir().unwrap();
    shroudforge_loader_config::config::update_loader(root.path(), |value| {
        value["modules"]["runtimeDiagnostics"]["enabled"] = json!(true);
        value["modules"]["runtimeDiagnostics"]["maximumDurationSeconds"] = json!(1);
        value["modules"]["runtimeDiagnostics"]["intervalMilliseconds"] = json!(60000);
        Ok(())
    })
    .unwrap();
    request(root.path(), "start").unwrap();
    let mut session = Session::new(root.path());
    session.measure(
        "mods",
        "example",
        "on_update",
        Duration::from_millis(30),
        true,
    );
    assert_eq!(
        crate::collectors::mod_callbacks::report(&session.measurements)["mods/example/on_update"]["slowCalls"],
        1
    );
    session.next_sample = Instant::now() + Duration::from_secs(60);
    session.started = Instant::now() - Duration::from_secs(2);
    session.tick();
    assert!(!session.active);
    assert_eq!(session.reason, "duration-complete");
    request(root.path(), "start").unwrap();
    session.next_config = Instant::now();
    session.tick();
    assert!(session.active);
    assert!(session.measurements.is_empty());
    request(root.path(), "stop").unwrap();
    session.next_config = Instant::now();
    session.tick();
    assert!(!session.active);
    assert_eq!(status(root.path())["reason"], "stopped");
}
#[test]
fn invalid_settings_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    assert!(
        shroudforge_loader_config::config::update_loader(root.path(), |value| {
            value["modules"]["runtimeDiagnostics"]["maximumDurationSeconds"] = json!(0);
            Ok(())
        })
        .is_err()
    );
    assert!(request(root.path(), "fake-action").is_err());
}

#[test]
fn file_based_jobs_load_their_lua_source_without_a_second_registration() {
    let root = tempfile::tempdir().unwrap();
    let directory = shroudforge_loader_config::paths::diagnostic_jobs_dir(root.path());
    std::fs::create_dir_all(&directory).unwrap();
    let definition = json!({"schemaVersion":1,"id":"user.editor-inputs",
        "name":"Editor input observation","description":"File-based input diagnostic",
        "capabilities":["queue"],
        "execution":{"kind":"lua","modId":"world-editor","script":"inputs.lua"}});
    std::fs::create_dir_all(directory.join("world-editor")).unwrap();
    let path = directory.join("world-editor/editor-inputs.json");
    let script = "return { on_update = function() diagnostic.finish() end }";
    std::fs::write(directory.join("inputs.lua"), script).unwrap();
    std::fs::write(&path, serde_json::to_vec(&definition).unwrap()).unwrap();
    let catalog = diagnostic_job_catalog(root.path(), &json!({"jobId":"user.editor-inputs"}));
    assert!(catalog.errors.is_empty());
    let custom = catalog
        .jobs
        .iter()
        .find(|job| job.id == "user.editor-inputs")
        .unwrap();
    let Some(DiagnosticExecution::Lua {
        mod_id,
        script: filename,
        source,
    }) = &custom.execution
    else {
        panic!("file-based Lua execution was not loaded");
    };
    assert_eq!(mod_id, "world-editor");
    assert_eq!(filename, "inputs.lua");
    assert_eq!(source, script);
    assert_eq!(custom.capabilities, ["queue"]);
    let serialized = serde_json::to_value(custom).unwrap();
    assert_eq!(serialized["schemaVersion"], 1);
    assert_eq!(serialized["execution"]["kind"], "lua");
    assert!(serialized["execution"].get("source").is_none());
    assert_eq!(
        catalog
            .jobs
            .iter()
            .filter(|job| job.id == "user.editor-inputs")
            .count(),
        1
    );
    let replacement = "return { on_update = function() diagnostic.publish({changed=true}) end }";
    std::fs::write(directory.join("inputs.lua"), replacement).unwrap();
    let catalog = diagnostic_job_catalog(root.path(), &json!({}));
    assert!(catalog.errors.is_empty());
    let custom = catalog
        .jobs
        .iter()
        .find(|job| job.id == "user.editor-inputs")
        .unwrap();
    assert!(matches!(&custom.execution,
        Some(DiagnosticExecution::Lua {source, ..}) if source == replacement));
}

#[test]
fn all_file_based_job_definitions_use_one_schema() {
    let root = tempfile::tempdir().unwrap();
    let catalog = diagnostic_job_catalog(root.path(), &json!({}));
    assert!(catalog.errors.is_empty());
    assert!(!catalog.jobs.is_empty());
    let schema = shroudforge_loader_config::Document {
        name: "diagnostic-job-test",
        schema: include_str!("../catalog/diagnostic-job.schema.json"),
        storage: shroudforge_loader_config::Storage::State("diagnostics"),
    };
    let mut ids = std::collections::HashSet::new();
    for job in catalog.jobs {
        assert!(
            ids.insert(job.id.clone()),
            "duplicate bundled job {}",
            job.id
        );
        assert_eq!(job.schema_version, 1);
        let definition = serde_json::to_value(job).unwrap();
        shroudforge_loader_config::config::validate_document(root.path(), schema, &definition)
            .unwrap();
    }
    let directory = shroudforge_loader_config::paths::diagnostic_jobs_dir(root.path());
    let action = json!({"schemaVersion":1,"id":"user.editor-action","name":"Editor action",
        "description":"Runs the active mod's existing action","capabilities":[],
        "execution":{"kind":"modAction","modId":"world-editor","action":"clear-selection","value":"test-value"}});
    shroudforge_loader_config::config::validate_document(root.path(), schema, &action).unwrap();
    std::fs::write(
        directory.join("editor-action.json"),
        serde_json::to_vec(&action).unwrap(),
    )
    .unwrap();
    let catalog = diagnostic_job_catalog(root.path(), &json!({}));
    assert!(catalog.errors.is_empty());
    let job = catalog
        .jobs
        .iter()
        .find(|job| job.id == "user.editor-action")
        .unwrap();
    assert!(
        matches!(&job.execution, Some(DiagnosticExecution::ModAction {mod_id, action, value})
        if mod_id == "world-editor" && action == "clear-selection" && value.as_deref() == Some("test-value"))
    );
}

#[test]
fn rust_system_dependency_job_is_selected_without_a_mod_or_source_file() {
    let root = tempfile::tempdir().unwrap();
    let catalog = diagnostic_job_catalog(root.path(), &json!({}));
    assert!(catalog.errors.is_empty(), "{:?}", catalog.errors);
    let job = catalog
        .jobs
        .iter()
        .find(|job| job.id == "inspect.system-dependencies")
        .expect("native ECS dependency job is bundled into the dropdown catalog");
    assert!(matches!(&job.execution,
        Some(DiagnosticExecution::Rust {action}) if action == "inspect-system-dependencies"));
    assert_eq!(job.execution.as_ref().unwrap().mod_id(), None);
}

#[test]
fn file_based_jobs_reject_unknown_fields_and_invalid_script_paths() {
    let root = tempfile::tempdir().unwrap();
    let directory = shroudforge_loader_config::paths::diagnostic_jobs_dir(root.path());
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("inputs.lua"), "return {}").unwrap();
    let definition = json!({"schemaVersion":1,"id":"user.editor-inputs","name":"Editor input",
        "description":"Schema validation","capabilities":[],
        "execution":{"kind":"lua","modId":"world-editor","script":"inputs.lua"}});
    let path = directory.join("editor-inputs.json");
    let mut invalid = Vec::new();
    let mut unknown = definition.clone();
    unknown["unexpected"] = json!(true);
    invalid.push(unknown);
    let mut unknown_execution = definition.clone();
    unknown_execution["execution"]["unexpected"] = json!(true);
    invalid.push(unknown_execution);
    let mut obsolete = definition.clone();
    obsolete["runtimeProbe"] = json!("world-editor.typed-inputs");
    invalid.push(obsolete);
    for script in [
        "",
        "../outside.lua",
        "..\\outside.lua",
        "/absolute.lua",
        "C:\\outside.lua",
        "nested/../../outside.lua",
        "inputs.txt",
    ] {
        let mut invalid_path = definition.clone();
        invalid_path["execution"]["script"] = json!(script);
        invalid.push(invalid_path);
    }
    for definition in invalid {
        std::fs::write(&path, serde_json::to_vec(&definition).unwrap()).unwrap();
        let catalog = diagnostic_job_catalog(root.path(), &json!({}));
        assert!(
            !catalog.errors.is_empty(),
            "accepted invalid job {definition}"
        );
        assert!(
            !catalog
                .jobs
                .iter()
                .any(|job| job.id == "user.editor-inputs")
        );
    }
}

#[test]
fn python_jobs_load_validated_sources_and_default_execution_options() {
    let root = tempfile::tempdir().unwrap();
    let directory = shroudforge_loader_config::paths::diagnostic_jobs_dir(root.path());
    std::fs::create_dir_all(directory.join("python")).unwrap();
    let source = "raise RuntimeError('catalog inspection must not execute Python')\n";
    std::fs::write(directory.join("python/capture.py"), source).unwrap();
    let definition = json!({"schemaVersion":1,"id":"user.python-capture","name":"Python capture",
        "description":"Loads source without starting a process","capabilities":[],
        "execution":{"kind":"python","script":"python/capture.py"}});
    let path = directory.join("python-capture.json");
    std::fs::write(&path, serde_json::to_vec(&definition).unwrap()).unwrap();
    let catalog = diagnostic_job_catalog(root.path(), &json!({}));
    assert!(catalog.errors.is_empty(), "{:?}", catalog.errors);
    let job = catalog
        .jobs
        .iter()
        .find(|job| job.id == "user.python-capture")
        .unwrap();
    let Some(DiagnosticExecution::Python {
        script,
        source: loaded,
        args,
        interpreter,
    }) = &job.execution
    else {
        panic!("Python execution must be loaded from the shared schema");
    };
    assert_eq!(script, "python/capture.py");
    assert_eq!(loaded, source);
    assert!(args.is_empty());
    assert!(interpreter.is_none());
    assert_eq!(job.execution.as_ref().unwrap().mod_id(), None);
    assert!(
        serde_json::to_value(job).unwrap()["execution"]
            .get("source")
            .is_none()
    );
    let mut definition = definition;
    definition["execution"]["args"] = json!(vec!["literal argument"; 64]);
    definition["execution"]["interpreter"] = json!("python");
    std::fs::write(&path, serde_json::to_vec(&definition).unwrap()).unwrap();
    let catalog = diagnostic_job_catalog(root.path(), &json!({}));
    assert!(catalog.errors.is_empty(), "{:?}", catalog.errors);
    let job = catalog
        .jobs
        .iter()
        .find(|job| job.id == "user.python-capture")
        .unwrap();
    assert!(
        matches!(&job.execution, Some(DiagnosticExecution::Python {args, interpreter, ..})
        if args.len() == 64 && interpreter.as_deref() == Some("python"))
    );
}

#[test]
fn python_jobs_reject_invalid_paths_missing_sources_and_argument_overflow() {
    let root = tempfile::tempdir().unwrap();
    let directory = shroudforge_loader_config::paths::diagnostic_jobs_dir(root.path());
    std::fs::create_dir_all(directory.join("python")).unwrap();
    std::fs::write(directory.join("python/capture.py"), "pass\n").unwrap();
    let definition = json!({"schemaVersion":1,"id":"user.python-capture","name":"Python capture",
        "description":"Shared Python validation","capabilities":[],
        "execution":{"kind":"python","script":"python/capture.py"}});
    let path = directory.join("python-capture.json");
    let mut invalid = Vec::new();
    for script in [
        "../capture.py",
        "..\\capture.py",
        "/capture.py",
        "C:\\capture.py",
        "python/capture.lua",
        "python/missing.py",
    ] {
        let mut entry = definition.clone();
        entry["execution"]["script"] = json!(script);
        invalid.push(entry);
    }
    let mut too_many = definition.clone();
    too_many["execution"]["args"] = json!(vec!["argument"; 65]);
    invalid.push(too_many);
    let mut too_long = definition.clone();
    too_long["execution"]["args"] = json!(["x".repeat(4097)]);
    invalid.push(too_long);
    let mut wrong_type = definition.clone();
    wrong_type["execution"]["args"] = json!([1]);
    invalid.push(wrong_type);
    let mut empty_interpreter = definition.clone();
    empty_interpreter["execution"]["interpreter"] = json!("");
    invalid.push(empty_interpreter);
    let mut long_interpreter = definition.clone();
    long_interpreter["execution"]["interpreter"] = json!("x".repeat(4097));
    invalid.push(long_interpreter);
    for definition in invalid {
        std::fs::write(&path, serde_json::to_vec(&definition).unwrap()).unwrap();
        let catalog = diagnostic_job_catalog(root.path(), &json!({}));
        assert!(
            !catalog.errors.is_empty(),
            "accepted invalid Python job {definition}"
        );
        assert!(
            !catalog
                .jobs
                .iter()
                .any(|job| job.id == "user.python-capture")
        );
    }
    std::fs::write(&path, serde_json::to_vec(&definition).unwrap()).unwrap();
    std::fs::write(
        directory.join("python/capture.py"),
        "x".repeat(1024 * 1024 + 1),
    )
    .unwrap();
    let catalog = diagnostic_job_catalog(root.path(), &json!({}));
    assert!(catalog.errors.iter().any(|reason| reason.contains("1 MiB")));
    assert!(
        !catalog
            .jobs
            .iter()
            .any(|job| job.id == "user.python-capture")
    );
}

#[derive(Default)]
struct JobCalls {
    starts: usize,
    updates: usize,
    stop_reasons: Vec<String>,
    requested_job: Option<String>,
    exports: Option<std::path::PathBuf>,
}

struct FakeDiagnosticHost {
    calls: std::rc::Rc<std::cell::RefCell<JobCalls>>,
    finish_after: usize,
    start_error: bool,
    update_error: bool,
}

impl FakeDiagnosticHost {
    fn new(finish_after: usize) -> Self {
        Self {
            calls: Default::default(),
            finish_after,
            start_error: false,
            update_error: false,
        }
    }
}

impl DiagnosticHost for FakeDiagnosticHost {
    fn start_job(&mut self, request: JobExecutionRequest) -> Result<Box<dyn RunningJob>, String> {
        let mut calls = self.calls.borrow_mut();
        calls.starts += 1;
        calls.requested_job = Some(request.job.id.clone());
        calls.exports = Some(request.exports.directory().into());
        assert!(
            matches!(&request.job.execution,
                Some(DiagnosticExecution::Lua {mod_id, ..}) if mod_id == "world-editor"
            ) || matches!(&request.job.execution, Some(DiagnosticExecution::Rust {action})
            if action == "inspect-system-dependencies")
        );
        if self.start_error {
            return Err("test host start failed".into());
        }
        Ok(Box::new(FakeRunningJob {
            calls: self.calls.clone(),
            finish_after: self.finish_after,
            update_error: self.update_error,
        }))
    }
}

struct FakeRunningJob {
    calls: std::rc::Rc<std::cell::RefCell<JobCalls>>,
    finish_after: usize,
    update_error: bool,
}

impl RunningJob for FakeRunningJob {
    fn update(&mut self, delta_seconds: f64) -> Result<JobOutput, String> {
        assert!(delta_seconds.is_finite() && delta_seconds >= 0.0);
        let mut calls = self.calls.borrow_mut();
        calls.updates += 1;
        if self.update_error {
            return Err("test job update failed".into());
        }
        Ok(JobOutput {
            report: json!({"state":"observing", "updates":calls.updates}),
            finished: calls.updates >= self.finish_after,
        })
    }

    fn stop(&mut self, reason: &str) -> Result<JobOutput, String> {
        let mut calls = self.calls.borrow_mut();
        calls.stop_reasons.push(reason.into());
        Ok(JobOutput {
            report: json!({"state":"stopped", "updates":calls.updates, "reason":reason}),
            finished: true,
        })
    }
}

fn runtime_job_session(action: &str, seconds: u64) -> (tempfile::TempDir, Session) {
    runtime_job_session_for(action, "world-editor.typed-inputs", seconds)
}

fn runtime_job_session_for(
    action: &str,
    job_id: &str,
    seconds: u64,
) -> (tempfile::TempDir, Session) {
    let root = tempfile::tempdir().unwrap();
    shroudforge_loader_config::config::update_loader(root.path(), |value| {
        let settings = &mut value["modules"]["runtimeDiagnostics"];
        settings["enabled"] = json!(true);
        settings["maximumDurationSeconds"] = json!(seconds);
        settings["intervalMilliseconds"] = json!(60000);
        Ok(())
    })
    .unwrap();
    request_job(root.path(), action, Some(job_id)).unwrap();
    let session = Session::new(root.path());
    (root, session)
}

#[test]
fn runtime_job_snapshot_starts_once_and_waits_for_finished() {
    let (root, mut session) = runtime_job_session("snapshot", 30);
    let mut host = FakeDiagnosticHost::new(3);
    session.tick_with_host(&mut host);
    assert!(session.active, "a Lua job is not a one-tick snapshot");
    assert_eq!(host.calls.borrow().starts, 1);
    assert_eq!(host.calls.borrow().updates, 1);
    assert!(host.calls.borrow().stop_reasons.is_empty());
    assert_eq!(
        host.calls.borrow().requested_job.as_deref(),
        Some("world-editor.typed-inputs")
    );
    assert_eq!(
        host.calls.borrow().exports.as_deref(),
        Some(JobExports::new(root.path(), "world-editor.typed-inputs").directory())
    );
    session.tick_with_host(&mut host);
    assert!(session.active);
    assert_eq!(host.calls.borrow().starts, 1);
    assert_eq!(host.calls.borrow().updates, 2);
    session.tick_with_host(&mut host);
    assert!(!session.active);
    assert_eq!(session.reason, "job-complete");
    assert_eq!(host.calls.borrow().stop_reasons, ["job-complete"]);
    assert_eq!(status(root.path())["reason"], "job-complete");
    assert_eq!(status(root.path())["report"]["execution"]["updates"], 3);
    assert_eq!(
        shroudforge_loader_config::config::read_loader(root.path()).unwrap()["modules"]["runtimeDiagnostics"]
            ["sessionActive"],
        false
    );
    session.tick_with_host(&mut host);
    drop(session);
    assert_eq!(host.calls.borrow().starts, 1);
    assert_eq!(host.calls.borrow().updates, 3);
    assert_eq!(host.calls.borrow().stop_reasons, ["job-complete"]);
}

#[test]
fn rust_job_uses_the_existing_snapshot_lifecycle() {
    let (_root, mut session) =
        runtime_job_session_for("snapshot", "inspect.system-dependencies", 30);
    let mut host = FakeDiagnosticHost::new(2);
    session.tick_with_host(&mut host);
    assert!(session.active);
    session.tick_with_host(&mut host);
    assert!(!session.active);
    assert_eq!(session.reason, "job-complete");
    let calls = host.calls.borrow();
    assert_eq!(calls.starts, 1);
    assert_eq!(
        calls.requested_job.as_deref(),
        Some("inspect.system-dependencies")
    );
    assert!(
        calls
            .exports
            .as_ref()
            .is_some_and(|path| path.ends_with("inspect.system-dependencies"))
    );
}

#[test]
fn runtime_job_manual_stop_releases_execution_exactly_once() {
    let (root, mut session) = runtime_job_session("start", 30);
    let mut host = FakeDiagnosticHost::new(usize::MAX);
    session.tick_with_host(&mut host);
    request(root.path(), "stop").unwrap();
    session.next_config = Instant::now();
    session.tick_with_host(&mut host);
    assert!(!session.active);
    assert_eq!(session.reason, "stopped");
    assert!(session.job_execution.is_none());
    assert_eq!(host.calls.borrow().stop_reasons, ["stopped"]);
    session.tick_with_host(&mut host);
    drop(session);
    assert_eq!(host.calls.borrow().starts, 1);
    assert_eq!(host.calls.borrow().updates, 1);
    assert_eq!(host.calls.borrow().stop_reasons, ["stopped"]);
}

#[test]
fn runtime_job_deadline_stops_without_waiting_for_sample_interval() {
    let (_root, mut session) = runtime_job_session("snapshot", 1);
    let mut host = FakeDiagnosticHost::new(usize::MAX);
    session.tick_with_host(&mut host);
    session.next_sample = Instant::now() + Duration::from_secs(60);
    session.started = Instant::now() - Duration::from_secs(2);
    session.tick_with_host(&mut host);
    assert!(!session.active);
    assert_eq!(session.reason, "duration-complete");
    assert_eq!(host.calls.borrow().updates, 1);
    assert_eq!(host.calls.borrow().stop_reasons, ["duration-complete"]);
    drop(session);
    assert_eq!(host.calls.borrow().stop_reasons.len(), 1);
}

#[test]
fn runtime_job_host_errors_finish_with_failure_and_keep_the_reason() {
    for fail_on_start in [true, false] {
        let (root, mut session) = runtime_job_session("snapshot", 30);
        let mut host = FakeDiagnosticHost::new(usize::MAX);
        host.start_error = fail_on_start;
        host.update_error = !fail_on_start;
        session.tick_with_host(&mut host);
        assert!(!session.active);
        assert_eq!(session.reason, "job-failed");
        let expected = if fail_on_start {
            "test host start failed"
        } else {
            "test job update failed"
        };
        assert_eq!(
            status(root.path())["report"]["execution"]["hostError"],
            expected
        );
        assert_eq!(host.calls.borrow().starts, 1);
        assert_eq!(
            host.calls.borrow().stop_reasons.len(),
            usize::from(!fail_on_start)
        );
        if !fail_on_start {
            assert_eq!(host.calls.borrow().stop_reasons, ["job-failed"]);
        }
        session.tick_with_host(&mut host);
        drop(session);
        assert_eq!(host.calls.borrow().starts, 1);
        assert_eq!(
            host.calls.borrow().stop_reasons.len(),
            usize::from(!fail_on_start)
        );
    }
}

#[test]
fn runtime_job_only_releases_the_matching_mod_owner() {
    let (_root, mut session) = runtime_job_session("start", 30);
    let mut host = FakeDiagnosticHost::new(usize::MAX);
    session.tick_with_host(&mut host);
    session.release_mod_host("sf-survival");
    assert!(session.active);
    assert!(session.job_execution.is_some());
    assert!(host.calls.borrow().stop_reasons.is_empty());
    session.release_mod_host("world-editor");
    assert!(!session.active);
    assert_eq!(session.reason, "runtime-host-stopped");
    assert!(session.job_execution.is_none());
    assert_eq!(host.calls.borrow().stop_reasons.len(), 1);
    session.release_mod_host("world-editor");
    session.release_runtime_host();
    session.tick_with_host(&mut host);
    drop(session);
    assert_eq!(host.calls.borrow().starts, 1);
    assert_eq!(host.calls.borrow().updates, 1);
    assert_eq!(host.calls.borrow().stop_reasons.len(), 1);
}

#[test]
fn diagnostic_exports_persist_and_keep_recovery_evidence_across_sessions() {
    let root = tempfile::tempdir().unwrap();
    let export_root = root.path().join("configured-exports");
    shroudforge_loader_config::config::update_loader(root.path(), |value| {
        value["paths"]["exports"] = json!(export_root);
        Ok(())
    })
    .unwrap();
    let exports = JobExports::new(root.path(), "world-editor.voxel-write-ab");
    assert_eq!(
        exports.directory(),
        export_root.join("diagnostics/world-editor.voxel-write-ab")
    );
    assert!(!exports.exists("baseline.json").unwrap());
    let baseline = r#"{"state":"baseline-saved","baseline":[65282,65282]}"#;
    exports.write("baseline.json", baseline).unwrap();
    exports
        .write("report.json", r#"{"state":"baseline-verify"}"#)
        .unwrap();
    exports
        .write("report.json", r#"{"state":"recovery-required"}"#)
        .unwrap();
    assert_eq!(
        exports.read("report.json").unwrap(),
        r#"{"state":"recovery-required"}"#
    );
    drop(exports);
    let reopened = JobExports::new(root.path(), "world-editor.voxel-write-ab");
    assert!(reopened.exists("baseline.json").unwrap());
    assert!(reopened.exists("report.json").unwrap());
    assert_eq!(reopened.read("baseline.json").unwrap(), baseline);
    assert_eq!(
        reopened.read("report.json").unwrap(),
        r#"{"state":"recovery-required"}"#
    );
}

#[test]
fn diagnostic_exports_reject_paths_nonfiles_and_oversized_data_without_replacing_evidence() {
    let root = tempfile::tempdir().unwrap();
    let exports = JobExports::new(root.path(), "world-editor.voxel-write-ab");
    exports
        .write("report.json", "previous recovery evidence")
        .unwrap();
    let maximum_name = "a".repeat(80);
    exports.write(&maximum_name, "valid boundary").unwrap();
    assert_eq!(exports.read(&maximum_name).unwrap(), "valid boundary");
    for name in [
        "",
        ".",
        "..",
        "../escape.json",
        "..\\escape.json",
        "folder/report.json",
        "folder\\report.json",
        "C:\\escape.json",
        "/absolute.json",
        "stream:alternate",
        "über.json",
        &"a".repeat(81),
    ] {
        assert!(exports.exists(name).is_err(), "exists accepted {name:?}");
        assert!(exports.read(name).is_err(), "read accepted {name:?}");
        assert!(
            exports.write(name, "replacement").is_err(),
            "write accepted {name:?}"
        );
    }
    let directory = exports.directory().join("folder");
    std::fs::create_dir(&directory).unwrap();
    assert!(exports.exists("folder").is_err());
    assert!(exports.read("folder").is_err());
    assert!(exports.write("folder", "replacement").is_err());
    assert!(directory.is_dir());
    exports
        .write("maximum.json", &"x".repeat(MAX_EXPORT_BYTES))
        .unwrap();
    assert_eq!(
        exports.read("maximum.json").unwrap().len(),
        MAX_EXPORT_BYTES
    );
    assert!(
        exports
            .write("report.json", &"x".repeat(MAX_EXPORT_BYTES + 1))
            .is_err()
    );
    let oversized = exports.directory().join("oversized.json");
    std::fs::File::create(&oversized)
        .unwrap()
        .set_len(MAX_EXPORT_BYTES as u64 + 1)
        .unwrap();
    assert!(exports.read("oversized.json").is_err());
    assert_eq!(
        exports.read("report.json").unwrap(),
        "previous recovery evidence"
    );
    assert!(
        !exports
            .directory()
            .parent()
            .unwrap()
            .join("escape.json")
            .exists()
    );
}

#[test]
#[ignore = "set SHROUDFORGE_DIAGNOSTICS_TEST_PYTHON"]
fn python_adapter_imports_arguments_reports_exit_and_cleans_up_after_stop() {
    let interpreter = std::env::var("SHROUDFORGE_DIAGNOSTICS_TEST_PYTHON").unwrap();
    let root = tempfile::tempdir().unwrap();
    shroudforge_loader_config::config::update_loader(root.path(), |value| {
        value["paths"]["exports"] = json!("configured-exports");
        value["paths"]["cache"] = json!("configured-cache");
        Ok(())
    })
    .unwrap();
    let diagnostic_exports = root.path().join("configured-exports").join("diagnostics");
    let cache = root.path().join("configured-cache").join("diagnostics");
    for (state, source) in [
        (
            "complete",
            "import json, sys\nfrom game_inspection.inspect.read_pe_image import PE\nwith open(sys.argv[1], 'w') as stream: json.dump(sys.argv[2:], stream)\nprint('ready', flush=True)\n",
        ),
        (
            "stopped",
            "import time\nprint('ready', flush=True)\ntime.sleep(60)\n",
        ),
        (
            "failed",
            "import sys\nprint('ready', flush=True)\nsys.exit(7)\n",
        ),
    ] {
        let id = format!("user.python-adapter.{state}");
        let exports = JobExports::new(root.path(), &id);
        let request = JobExecutionRequest {
            job: DiagnosticJob {
                schema_version: 1,
                id,
                name: "Python adapter contract".into(),
                description: "Private interpreter fixture without engine calls".into(),
                capabilities: vec![],
                execution: Some(DiagnosticExecution::Python {
                    script: "adapter_contract.py".into(),
                    source: source.into(),
                    args: vec![
                        "{exports}/argv.json".into(),
                        "literal argument".into(),
                        "{diagnosticExports}".into(),
                    ],
                    interpreter: Some(interpreter.clone()),
                }),
            },
            game_root: root.path().to_owned(),
            exports: exports.clone(),
        };
        let mut execution = crate::run_python_diagnostic_job::start(root.path(), request).unwrap();
        let cached_sources: Vec<_> = std::fs::read_dir(&cache)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(cached_sources.len(), 1);
        assert!(cached_sources[0].join("adapter_contract.py").is_file());
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut failed = false;
        let output = loop {
            assert!(
                Instant::now() < deadline,
                "Python adapter did not reach {state}"
            );
            match execution.update(0.01) {
                Ok(output) if output.finished => break output,
                Err(error) => {
                    assert_eq!(state, "failed", "{error}");
                    failed = true;
                    break execution.update(0.0).unwrap();
                }
                _ if state == "stopped"
                    && exports.read("stdout.log").unwrap().contains("ready") =>
                {
                    break execution.stop("integration-test-stop").unwrap();
                }
                _ => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        assert!(output.finished);
        assert_eq!(
            failed,
            state == "failed",
            "nonzero exit must fail the shared lifecycle"
        );
        assert_eq!(output.report["state"], state);
        assert_eq!(
            exports.read("report.json").unwrap(),
            output.report.to_string()
        );
        assert!(exports.read("stdout.log").unwrap().contains("ready"));
        if state == "complete" {
            assert_eq!(output.report["exitCode"], 0);
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&exports.read("argv.json").unwrap())
                    .unwrap(),
                json!(["literal argument", diagnostic_exports])
            );
        } else if state == "failed" {
            assert_eq!(output.report["exitCode"], 7);
        } else {
            assert_eq!(output.report["reason"], "integration-test-stop");
        }
        let report = exports.read("report.json").unwrap();
        let stdout = exports.read("stdout.log").unwrap();
        assert!(execution.update(0.0).unwrap().finished);
        assert!(execution.stop("repeated-stop").unwrap().finished);
        drop(execution);
        assert!(cached_sources.iter().all(|path| !path.exists()));
        assert_eq!(exports.read("report.json").unwrap(), report);
        assert_eq!(exports.read("stdout.log").unwrap(), stdout);
        assert!(
            std::fs::read_dir(exports.directory())
                .unwrap()
                .all(|entry| !entry.unwrap().file_type().unwrap().is_dir())
        );
    }
}

// Kiln layer (dev29 inbox 50 item 7): the status is written when something other than `updatedAt`
// changed, or when the last write is UNCHANGED_STATUS_REFRESH old, and not on every tick.
#[test]
fn unchanged_status_is_written_only_to_stay_fresh() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::new(root.path());
    let state = shroudforge_loader_config::paths::state_file(root.path());
    let mark = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let set_mark = || {
        std::fs::File::options()
            .write(true)
            .open(&state)
            .unwrap()
            .set_modified(mark)
            .unwrap()
    };
    let modified = || std::fs::metadata(&state).unwrap().modified().unwrap();
    assert!(status(root.path())["fresh"].as_bool().unwrap());

    set_mark();
    session.publish();
    session.publish();
    assert_eq!(modified(), mark, "nothing but updatedAt changed: no write");

    session.reason = "stopped";
    session.publish();
    assert_ne!(modified(), mark, "a changed reason is written at once");
    assert_eq!(status(root.path())["reason"], "stopped");

    // updatedAt has whole seconds: the same second would be an unchanged section, skipped as such.
    std::thread::sleep(Duration::from_millis(1100));
    set_mark();
    let (content, _) = session.published.clone().unwrap();
    session.published = Some((
        content,
        Instant::now() - crate::publish_diagnostic_status::UNCHANGED_STATUS_REFRESH,
    ));
    session.publish();
    assert_ne!(modified(), mark, "an unchanged status is refreshed before readers call it stale");
    assert!(
        crate::publish_diagnostic_status::UNCHANGED_STATUS_REFRESH.as_secs() * 2 <= 65,
        "status() allows 65 s"
    );
}
