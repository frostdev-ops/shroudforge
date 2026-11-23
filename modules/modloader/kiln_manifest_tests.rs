// SPDX-License-Identifier: MIT
//! Kiln layer: EML manifests with null optional metadata, and schema errors that name the field
//! (0.2.0-dev.30.6 report: `"description": null` refused with "mod: null is not of type string").

use crate::path_types::PathBuf;
use crate::{ModRegistry, read_mod_manifest::read_manifest_path};
use serde_json::json;
use std::{fs, path::Path};

struct Folder(std::path::PathBuf);
impl Drop for Folder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `<temp>/shroudforge/mods/<id>/mod.json` for each package.
fn mods(packages: &[(&str, serde_json::Value)]) -> (Folder, PathBuf) {
    let root = std::env::temp_dir().join(format!("sf-kiln-manifest-{}", uuid_like()));
    let dir = root.join("shroudforge/mods");
    for (id, manifest) in packages {
        fs::create_dir_all(dir.join(id)).unwrap();
        fs::write(dir.join(id).join("mod.json"), serde_json::to_vec(manifest).unwrap()).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();
    let utf8 = PathBuf::from_path_buf(dir).unwrap();
    (Folder(root), utf8)
}

fn uuid_like() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    )
}

/// What EML's own manifest type (`mod-loader-base` `ModManifest`) reads as absent: every `Option` field.
fn eml_manifest_with_nulls(id: &str) -> serde_json::Value {
    json!({
        "id": id, "name": "Null Metadata", "version": "1.0.0", "capabilities": ["patch"],
        "description": null, "license": null, "icon": null,
        "dependencies": [{"id": "other", "version": "^1.0.0", "optional": null}]
    })
}

#[test]
fn eml_optional_metadata_may_be_null() {
    let (_temp, dir) = mods(&[("null-metadata", eml_manifest_with_nulls("null-metadata"))]);
    let manifest = read_manifest_path(Path::new("."), dir.join("null-metadata").as_std_path())
        .expect("EML reads null optional metadata as absent");
    assert_eq!(manifest.description, None);
    assert_eq!(manifest.license, None);
    assert_eq!(manifest.icon, None);
    assert_eq!(manifest.dependencies[0].optional, None);
    let registry = ModRegistry::load(&dir).unwrap_or_else(|report| {
        panic!("{:?}", report.mods.iter().map(|m| m.error.to_string()).collect::<Vec<_>>())
    });
    assert!(registry.mods.contains_key("null-metadata"));
}

#[test]
fn schema_errors_name_the_field() {
    let mut bad_name = eml_manifest_with_nulls("bad-name");
    bad_name["name"] = json!(false);
    let mut bad_dependency = eml_manifest_with_nulls("bad-dependency");
    bad_dependency["dependencies"][0]["optional"] = json!("yes");
    let (_temp, dir) = mods(&[("bad-name", bad_name), ("bad-dependency", bad_dependency)]);
    let report = match ModRegistry::load(&dir) {
        Ok(_) => panic!("both packages are refused"),
        Err(report) => report,
    };
    let mut errors: Vec<_> = report.mods.iter().map(|m| m.error.to_string()).collect();
    errors.sort();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors[0].ends_with(r#"mod.json: mod: /dependencies/0/optional: "yes" is not of types "boolean", "null""#), "{}", errors[0]);
    assert!(errors[1].ends_with(r#"mod.json: mod: /name: false is not of type "string""#), "{}", errors[1]);
    // An error of the whole document has no field to name.
    let mut missing = eml_manifest_with_nulls("missing-id");
    missing.as_object_mut().unwrap().remove("id");
    let package = dir.join("missing-id");
    fs::create_dir_all(&package).unwrap();
    fs::write(package.join("mod.json"), serde_json::to_vec(&missing).unwrap()).unwrap();
    let error = read_manifest_path(Path::new("."), package.as_std_path()).unwrap_err();
    assert_eq!(error, r#"mod: "id" is a required property"#);
}
