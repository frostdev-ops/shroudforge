// SPDX-License-Identifier: MIT
//! Kiln layer: the packages a host has off for this start (`shroudforge_host_skip_packages`,
//! `kiln/abi/host_skip.h`): mod.json ids, or the folder name of a package folder without mod.json (a DLL the DLL
//! Manager loads by its native-plugin.ini or a compatibility profile). Kiln hosts this runtime for a mod profile. A package folder the player put into the
//! mods folder himself is his, and Kiln never writes into it, so ShroudForge's own switch in its `extended.mod.json`
//! may be on while the profile has the mod off. The host names such packages here before the startup pass, and every
//! manifest read in this process ([`crate::read_manifest`]) then has them switched off: the startup asset pass, the
//! DLL Manager's plan and the live runtime leave them out. Nothing on disk changes, and a process whose host names
//! none (ShroudForge's own bootstrap) behaves as upstream.

use std::collections::BTreeSet;

use parking_lot::Mutex;

/// The ids a host named, and whether the startup pass has begun (then the list is fixed for the process).
#[derive(Default)]
pub struct HostSkip {
    inner: Mutex<(BTreeSet<String>, bool)>,
}

impl HostSkip {
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new((BTreeSet::new(), false)),
        }
    }

    /// Adds `ids` (mod.json ids). False, and nothing added, when one is not a valid id or the startup pass has begun
    /// ([`HostSkip::seal`]): a package the pass may already have read cannot be left out consistently any more.
    pub fn add<'a>(&self, ids: impl IntoIterator<Item = &'a str>) -> bool {
        let ids: Vec<&str> = ids.into_iter().collect();
        if ids.is_empty() || !ids.iter().all(|id| crate::valid_id(id)) {
            return false;
        }
        let mut inner = self.inner.lock();
        if inner.1 {
            return false;
        }
        inner.0.extend(ids.into_iter().map(str::to_owned));
        true
    }

    pub fn contains(&self, id: &str) -> bool {
        self.inner.lock().0.contains(id)
    }

    /// The startup pass begins: no more ids are taken. The ids, sorted, for the log.
    pub fn seal(&self) -> Vec<String> {
        let mut inner = self.inner.lock();
        inner.1 = true;
        inner.0.iter().cloned().collect()
    }
}

/// This process's list.
pub static HOST_SKIP: HostSkip = HostSkip::new();

/// Is the package `id` off for this start at the host's request?
pub fn is_skipped(id: &str) -> bool {
    HOST_SKIP.contains(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read_mod_manifest::read_manifest_path;
    use std::fs;

    struct Folder(std::path::PathBuf);
    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn game(name: &str) -> (Folder, std::path::PathBuf) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "sf-kiln-host-skip-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(shroudforge_loader_config::paths::config_dir(&root)).unwrap();
        (Folder(root.clone()), root)
    }

    /// `<mods>/<dir>`: a runtime package with a DLL its native-plugin.ini names, ShroudForge's switch on for both sides.
    fn package(root: &std::path::Path, dir: &str, id: &str) -> std::path::PathBuf {
        let p = shroudforge_loader_config::paths::mods_dir(root).join(dir);
        fs::create_dir_all(p.join("src")).unwrap();
        fs::write(
            p.join("mod.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": id, "name": id, "version": "1.0.0", "capabilities": ["runtime"]
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            p.join("extended.mod.json"),
            br#"{"schemaVersion":1,"launcher":"EML","enabled":true,"targets":["client","server"]}"#,
        )
        .unwrap();
        fs::write(p.join("src/mod.lua"), "return {}\n").unwrap();
        fs::write(p.join(format!("{dir}.dll")), b"MZ").unwrap();
        fs::write(
            p.join("native-plugin.ini"),
            format!("[Plugin]\nEnabled=true\nDll={dir}.dll\n"),
        )
        .unwrap();
        p
    }

    #[test]
    fn a_sealed_list_takes_no_more_ids() {
        let list = HostSkip::new();
        assert!(list.add(["quiver", "stacks"]));
        assert!(
            list.contains("quiver") && !list.contains("Quiver"),
            "ids are matched as ShroudForge's registry matches them"
        );
        assert!(!list.add(["../escape"]), "not an id");
        assert!(!list.add([]), "nothing named");
        assert_eq!(list.seal(), ["quiver", "stacks"]);
        assert!(!list.add(["late"]), "after the startup pass began");
        assert!(!list.contains("late"));
    }

    /// The player's own folder of a mod the host's profile has off: switched on in its own file, off in this process,
    /// with its file as it was.
    #[test]
    fn a_skipped_package_is_off_in_the_manifest_the_plan_and_the_dll_plan() {
        let (_temp, root) = game("plan");
        let own = package(&root, "kiln-test-own-folder", "kiln-test-own-folder");
        package(&root, "kiln-test-other", "kiln-test-other");
        let before = fs::read(own.join("extended.mod.json")).unwrap();
        assert!(
            read_manifest_path(&root, &own).unwrap().enabled,
            "its own switch is on"
        );
        assert!(HOST_SKIP.add(["kiln-test-own-folder"]));

        assert!(!read_manifest_path(&root, &own).unwrap().enabled);
        let environment =
            crate::ModEnvironment::load(camino::Utf8Path::from_path(&root).unwrap()).unwrap();
        assert!(!environment.is_mod_enabled("kiln-test-own-folder"));
        assert!(environment.is_mod_enabled("kiln-test-other"));
        let plan: Vec<_> = environment
            .plan(false, "1.8.1")
            .iter()
            .map(|m| m.info().id.clone())
            .collect();
        assert!(
            !plan.contains(&"kiln-test-own-folder".to_owned())
                && plan.contains(&"kiln-test-other".to_owned()),
            "{plan:?}"
        );
        let dlls: Vec<_> = crate::dll_manager::resolve_load_plan(&root, false)
            .entries
            .into_iter()
            .map(|e| e.package_directory)
            .collect();
        assert_eq!(
            dlls,
            ["kiln-test-other"],
            "the DLL Manager loads no DLL of a package that is off"
        );
        assert_eq!(
            fs::read(own.join("extended.mod.json")).unwrap(),
            before,
            "nothing on disk changes"
        );
    }

    /// A folder without mod.json (a DLL its native-plugin.ini names): named by its folder.
    #[test]
    fn a_skipped_folder_without_a_manifest_loads_no_dll() {
        let (_temp, root) = game("dll-only");
        for dir in ["kiln-test-dll-only", "kiln-test-dll-kept"] {
            let p = shroudforge_loader_config::paths::mods_dir(&root).join(dir);
            fs::create_dir_all(&p).unwrap();
            fs::write(p.join(format!("{dir}.dll")), b"MZ").unwrap();
            fs::write(
                p.join("native-plugin.ini"),
                format!("[Plugin]\nEnabled=true\nDll={dir}.dll\n"),
            )
            .unwrap();
        }
        let planned = || -> Vec<String> {
            crate::dll_manager::resolve_load_plan(&root, false)
                .entries
                .into_iter()
                .map(|e| e.package_directory)
                .collect()
        };
        assert_eq!(planned(), ["kiln-test-dll-kept", "kiln-test-dll-only"]);
        assert!(HOST_SKIP.add(["kiln-test-dll-only"]));
        assert_eq!(planned(), ["kiln-test-dll-kept"]);
    }
}
