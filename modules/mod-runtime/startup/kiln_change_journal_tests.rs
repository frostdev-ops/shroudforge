// SPDX-License-Identifier: MIT
//! Kiln layer: the pregame pass's change journal (kiln_change_journal.rs): the member compare, paths moving with later
//! insertions and removals, the per-package record of a pass (snapshots as the runner gives them), its file.
use super::*;
use kfc::guid::Guid;

fn st(fields: &[(&str, Value)]) -> Value {
    Value::Struct(Box::new(fields.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()))
}
fn recipe(duration: f64, count: u64) -> Value {
    st(&[("craftingDuration", Value::Float(duration)), ("input", Value::Array(vec![st(&[("count", Value::UInt(count))])]))])
}
fn list(n: usize) -> Value {
    st(&[("recipes", Value::Array((0..n).map(|i| recipe(i as f64, 1)).collect()))])
}
fn with_recipes(v: &Value, f: impl FnOnce(&mut Vec<Value>)) -> Value {
    let mut v = v.clone();
    if let Value::Struct(s) = &mut v
        && let Some(Value::Array(r)) = s.get_mut("recipes")
    {
        f(r);
    }
    v
}
fn paths(d: &Diff) -> Vec<String> {
    let mut v: Vec<String> = d.changed.iter().map(|p| text(p)).collect();
    v.sort();
    v
}
fn id(n: u8) -> ResourceId {
    let mut bytes = [0u8; 16];
    bytes[0] = n;
    ResourceId::new(Guid::from(bytes), 0x1234_5678, 0)
}
// The fixture's "encoding": the value's debug text (what matters is that equal values give equal bytes).
fn bytes(v: &Value) -> Vec<u8> {
    format!("{v:?}").into_bytes()
}
fn snap(n: u8, v: &Value, created: bool) -> Snapshot {
    Snapshot { id: id(n), now: Some((v.clone(), bytes(v))), created }
}

#[test]
fn only_members_that_differ_are_named() {
    let a = st(&[("maxStackSize", Value::UInt(10)), ("weight", Value::Float(1.5))]);
    assert!(diff(&a, &a.clone(), 100).changed.is_empty());
    let b = st(&[("maxStackSize", Value::UInt(100)), ("weight", Value::Float(1.5))]);
    assert_eq!(paths(&diff(&a, &b, 100)), ["maxStackSize"]);
    let n = st(&[("f", Value::Float(f64::NAN))]);
    assert!(diff(&n, &n.clone(), 100).changed.is_empty());
}

#[test]
fn a_list_of_another_length_is_aligned_by_its_ends_and_earlier_paths_move() {
    let d = diff(&list(3), &list(4), 100);
    assert_eq!(paths(&d), ["recipes[3]", "recipes[]"]);
    let mut p = Paths::default();
    let one = with_recipes(&list(3), |r| r[2] = recipe(5.0, 1));
    p.changed(1, diff(&list(3), &one, 100));
    assert_eq!(p.of(1), ["recipes[2].craftingDuration"]);
    let two = with_recipes(&one, |r| r.insert(0, recipe(9.0, 9)));
    p.changed(2, diff(&one, &two, 100));
    assert_eq!(p.of(1), ["recipes[3].craftingDuration"], "moved with the insertion in front");
    assert_eq!(p.of(2), ["recipes[0]", "recipes[]"]);
    let three = with_recipes(&two, |r| {
        r.remove(3);
    });
    p.changed(3, diff(&two, &three, 100));
    assert!(p.of(1).is_empty(), "left with the removed element");
    let mut q = Paths::default();
    q.changed(1, diff(&Value::UInt(1), &Value::String("x".into()), 100));
    assert_eq!(q.of(1), [""], "another kind at the root: the whole resource");
    let many = st(&(0..10).map(|i| (Box::leak(format!("f{i}").into_boxed_str()) as &str, Value::UInt(i))).collect::<Vec<_>>());
    let more = st(&(0..10).map(|i| (Box::leak(format!("f{i}").into_boxed_str()) as &str, Value::UInt(i + 1))).collect::<Vec<_>>());
    let mut r = Paths::default();
    r.changed(1, diff(&many, &more, 4));
    assert_eq!(r.of(1), [""], "past the limit: the whole resource");
}

// A pass of three packages as the runner records it: alpha changes R1's stack size; bravo writes R1 back as it was (no
// change: not named) and appends a recipe to R2; charlie creates R3 and slows the first recipe; a package that only read
// is listed with nothing. The record is format 2 of the ShroudForge dialect, hosted writer, bound to its generation.
#[test]
fn a_pass_names_each_package_where_it_changed_something() {
    let j = ChangeJournal::default();
    let item = st(&[("maxStackSize", Value::UInt(10))]);
    let archive: HashMap<ResourceId, Vec<u8>> = [(id(1), bytes(&item)), (id(2), bytes(&list(2)))].into_iter().collect();
    let values: HashMap<Vec<u8>, Value> = [(bytes(&item), item.clone()), (bytes(&list(2)), list(2))].into_iter().collect();
    let known = std::cell::RefCell::new(values);
    let read = |rid: &ResourceId| archive.get(rid).cloned();
    let decode = |_: &ResourceId, b: &[u8]| known.borrow().get(b).cloned();
    j.begin("alpha", Some("1.0.0".into()));
    let item100 = st(&[("maxStackSize", Value::UInt(100))]);
    known.borrow_mut().insert(bytes(&item100), item100.clone());
    j.end(vec![snap(1, &item100, false)], read, decode);
    j.begin("bravo", None);
    let appended = with_recipes(&list(2), |r| r.push(recipe(7.0, 2)));
    known.borrow_mut().insert(bytes(&appended), appended.clone());
    j.end(vec![snap(1, &item100, false), snap(2, &appended, false)], read, decode);
    j.begin("charlie", Some("2.0.0".into()));
    j.created(id(3), false);
    j.content(ContentHash::from_data(b"blob"));
    j.content(ContentHash::from_data(b"blob"));
    let slowed = with_recipes(&appended, |r| r[0] = recipe(9.5, 1));
    known.borrow_mut().insert(bytes(&slowed), slowed.clone());
    j.end(vec![snap(1, &item100, false), snap(2, &slowed, false), snap(3, &item, true)], read, decode);
    j.begin("reader", None);
    j.end(vec![snap(1, &item100, false), snap(2, &slowed, false), snap(3, &item, true)], read, decode);
    let text = j.json("enshrouded", &"ab".repeat(32), "retry", &[("broken".into(), Some("0.9.0".into()), "line one\nline two".into())]);
    // for Kiln's schema test of the format (schema/tests/sfi_eml_journal.mjs with EML_JOURNAL_DIR): the record as written
    if let Some(dir) = std::env::var_os("EML_JOURNAL_OUT") {
        fs::create_dir_all(&dir).unwrap();
        fs::write(std::path::Path::new(&dir).join("hosted-pass.json"), &text).unwrap();
    }
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!((&doc["format"], &doc["dialect"], &doc["writer"]), (&serde_json::json!(2), &serde_json::json!("shroudforge"), &serde_json::json!("shroudforge")));
    assert_eq!(doc["generation"], "ab".repeat(32));
    assert_eq!(doc["receiptSha256"], doc["generation"], "the hosted writer keeps no receipt");
    assert_eq!(doc["attempt"], "retry");
    let ids: Vec<&str> = doc["packages"].as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["alpha", "bravo", "charlie", "reader"]);
    let pkg = |i: usize| &doc["packages"][i];
    assert_eq!(pkg(0)["version"], "1.0.0");
    assert_eq!(pkg(0)["changed"][0]["paths"], serde_json::json!(["maxStackSize"]));
    assert_eq!(pkg(1)["changed"].as_array().unwrap().len(), 1, "bravo wrote R1 back as it was: not named there: {}", pkg(1));
    assert_eq!(pkg(1)["changed"][0]["paths"], serde_json::json!(["recipes[2]", "recipes[]"]));
    assert_eq!(pkg(1)["version"], serde_json::Value::Null);
    assert_eq!(pkg(2)["changed"][0]["paths"], serde_json::json!(["recipes[0].craftingDuration"]));
    assert_eq!(pkg(2)["created"][0]["ordinal"], 1);
    assert_eq!(pkg(2)["created"][0]["explicit"], false);
    assert_eq!(pkg(2)["contents"].as_array().unwrap().len(), 1, "a content once per package");
    assert_eq!(pkg(3)["changed"], serde_json::json!([]));
    assert_eq!(doc["notApplied"], serde_json::json!([{"id": "broken", "version": "0.9.0", "kind": "shroudforge", "contentSha256": null, "reason": "line one line two"}]));
    assert_eq!(doc["truncated"], false);
    // field order as the format writes it
    let at = |k: &str| text.find(&format!("\"{k}\":")).unwrap();
    assert!(at("format") < at("stem") && at("stem") < at("generation") && at("attempt") < at("packages") && at("unattributed") < at("writer") && at("writer") < at("notApplied"));
    // a new attempt drops what the last one recorded
    j.reset();
    let empty: serde_json::Value = serde_json::from_str(&j.json("enshrouded", &"ab".repeat(32), "first", &[])).unwrap();
    assert_eq!(empty["packages"], serde_json::json!([]));
}

// Another package writing into a created resource names its creator; past the budget the rest is no package's.
#[test]
fn a_write_into_another_packages_creation_names_its_creator() {
    let j = ChangeJournal::default();
    let item = st(&[("maxStackSize", Value::UInt(1))]);
    let item2 = st(&[("maxStackSize", Value::UInt(2))]);
    let known: HashMap<Vec<u8>, Value> = [(bytes(&item), item.clone()), (bytes(&item2), item2.clone())].into_iter().collect();
    j.begin("maker", None);
    j.created(id(9), true);
    j.end(vec![snap(9, &item, true)], |_| None, |_, b| known.get(b).cloned());
    j.begin("user", None);
    j.end(vec![snap(9, &item2, true)], |_| None, |_, b| known.get(b).cloned());
    let doc: serde_json::Value = serde_json::from_str(&j.json("enshrouded", &"cd".repeat(32), "first", &[])).unwrap();
    assert_eq!(doc["packages"][0]["changed"], serde_json::json!([]), "the creator's own writes are not listed");
    assert_eq!(doc["packages"][0]["created"][0]["explicit"], true);
    assert_eq!(doc["packages"][1]["changed"][0]["createdBy"], serde_json::json!({"package": "maker", "ordinal": 1}));
    assert_eq!(doc["packages"][1]["changed"][0]["paths"], serde_json::json!(["maxStackSize"]));
}

#[test]
fn past_the_budget_what_changed_is_no_packages_and_the_record_is_truncated() {
    let j = ChangeJournal::default();
    j.state().spent = BUDGET; // as if the comparing so far had used it all
    let item = st(&[("maxStackSize", Value::UInt(1))]);
    let item2 = st(&[("maxStackSize", Value::UInt(2))]);
    j.begin("first", None);
    j.end(vec![snap(1, &item2, false)], |_| Some(bytes(&item)), |_, _| Some(item.clone()));
    assert!(j.exhausted());
    j.begin("later", None);
    j.end(vec![], |_| None, |_, _| None);
    j.finish(vec![snap(1, &item2, false), snap(2, &item, false)], |rid| if *rid == id(2) { Some(bytes(&item)) } else { Some(bytes(&item)) });
    let doc: serde_json::Value = serde_json::from_str(&j.json("enshrouded", &"ef".repeat(32), "first", &[])).unwrap();
    assert_eq!(doc["truncated"], true);
    assert_eq!(doc["unattributed"]["truncated"], true);
    let un = doc["unattributed"]["changed"].as_array().unwrap();
    assert_eq!(un.len(), 0, "the first package's change was compared and named: {doc}");
    assert_eq!(doc["packages"][0]["changed"][0]["paths"], serde_json::json!(["maxStackSize"]));
    // a change after the stop: no package's, whole
    let j2 = ChangeJournal::default();
    j2.state().spent = BUDGET + BUDGET;
    j2.state().exhausted = true;
    j2.begin("late", None);
    j2.end(vec![snap(1, &item2, false)], |_| None, |_, _| None);
    j2.finish(vec![snap(1, &item2, false)], |_| Some(bytes(&item)));
    let doc: serde_json::Value = serde_json::from_str(&j2.json("enshrouded", &"ef".repeat(32), "first", &[])).unwrap();
    assert_eq!(doc["unattributed"]["changed"][0]["paths"], serde_json::json!([""]));
    assert_eq!(doc["packages"][0]["changed"], serde_json::json!([]));
}

// The file: written after a pass beside <stem>.kfc, bound to that file's SHA-256, removed by forget (none is fine).
#[test]
fn the_file_is_bound_to_the_published_header() {
    let root = std::env::temp_dir().join(format!("sf-kiln-journal-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("enshrouded.kfc"), b"abc").unwrap();
    let j = ChangeJournal::default();
    j.begin("alpha", None);
    j.end(vec![], |_| None, |_, _| None);
    write(&root, "enshrouded", &j, "first", &[]).unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&fs::read(journal_path(&root, "enshrouded")).unwrap()).unwrap();
    assert_eq!(doc["generation"], "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert!(!root.join("enshrouded.kfc.change-journal.json.partial").exists());
    forget(&root, "enshrouded").unwrap();
    assert!(!journal_path(&root, "enshrouded").exists());
    forget(&root, "enshrouded").unwrap();
    fs::remove_dir_all(&root).unwrap();
}

// Kiln review (dev.32 review-j3): each package carries a digest of its files (contentSha256), so Kiln's effect replay matches
// a recorded effect by id, version AND files: the files the pass fingerprint hashes (a dot-file and a log are output, not
// input), framed as Kiln's other package digests. A package the loader gave no digest for keeps null.
#[test]
fn each_package_carries_the_digest_of_its_files() {
    let root = std::env::temp_dir().join(format!("sf-journal-digest-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let pkg = root.join("alpha");
    fs::create_dir_all(pkg.join("src")).unwrap();
    fs::write(pkg.join("mod.json"), br#"{"id":"alpha","version":"1.0.0"}"#).unwrap();
    fs::write(pkg.join("src").join("main.lua"), b"return {}").unwrap();
    let d1 = package_digest(&pkg).unwrap();
    assert_eq!(d1.len(), 64);
    assert!(d1.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()), "{d1}");
    fs::write(pkg.join("lantern.log"), b"written after the pass").unwrap();
    fs::create_dir_all(pkg.join(".git")).unwrap();
    fs::write(pkg.join(".git").join("HEAD"), b"ref").unwrap();
    assert_eq!(package_digest(&pkg).unwrap(), d1, "a log and a dot-entry are no input, as for the pass fingerprint");
    fs::write(pkg.join("src").join("main.lua"), b"return { changed = true }").unwrap();
    let d2 = package_digest(&pkg).unwrap();
    assert_ne!(d2, d1, "a changed file is another digest");
    fs::rename(pkg.join("src").join("main.lua"), pkg.join("src").join("other.lua")).unwrap();
    assert_ne!(package_digest(&pkg).unwrap(), d2, "so is a renamed one");
    assert!(package_digest(&root.join("missing")).is_err());
    let _ = fs::remove_dir_all(&root);
    let j = ChangeJournal::default();
    j.begin("alpha", Some("1.0.0".into()));
    j.end(Vec::new(), |_| None, |_, _| None);
    j.begin("beta", None);
    j.end(Vec::new(), |_| None, |_, _| None);
    j.digests([("alpha".to_owned(), "ab".repeat(32)), ("broken".to_owned(), "cd".repeat(32))].into_iter().collect());
    let doc: serde_json::Value =
        serde_json::from_str(&j.json("enshrouded", &"ef".repeat(32), "retry", &[("broken".into(), None, "boom".into())])).unwrap();
    assert_eq!(doc["packages"][0]["contentSha256"], "ab".repeat(32));
    assert_eq!(doc["packages"][1]["contentSha256"], serde_json::Value::Null, "no digest given: null");
    assert_eq!(doc["notApplied"][0]["contentSha256"], "cd".repeat(32));
}

// Kiln review (review-j3): encoding the edited resources after each entrypoint is part of the journal's cost, so it counts
// toward BUDGET, and past the budget it is not done at all (before, only the comparing was timed: a large modpack paid an
// encode of every edited resource after every package however long that took).
#[test]
fn the_encoding_counts_toward_the_budget_and_stops_with_it() {
    let j = ChangeJournal::default();
    j.state().spent = BUDGET;
    j.state().exhausted = true;
    j.begin("late", None);
    let called = std::cell::Cell::new(false);
    j.end_with(|| { called.set(true); Vec::new() }, |_| None, |_, _| None);
    assert!(!called.get(), "past the budget no resource is encoded for the journal");
    assert!(j.state().current.is_none(), "and the package's entrypoint is over");
    let k = ChangeJournal::default();
    k.state().spent = BUDGET - std::time::Duration::from_millis(1);
    k.begin("slow", None);
    k.end_with(|| { std::thread::sleep(std::time::Duration::from_millis(20)); Vec::new() }, |_| None, |_, _| None);
    assert!(k.exhausted(), "an encoding that takes the journal past its budget stops it");
}
