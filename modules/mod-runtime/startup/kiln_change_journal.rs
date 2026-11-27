// SPDX-License-Identifier: MIT
//! Kiln layer: the change journal of the pregame pass, so that Kiln (which hosts this runtime) can name each package
//! on the registry fields it changed (Kiln's Conflicts view and `kiln/state/registry.json`), and replay a package's
//! recorded effect after a game update when its owner opts in (Kiln's effect replay). Never a value is kept in the
//! record; nothing here changes what the pass writes.
//!
//! **What it records.** Per package whose entrypoint ran, in run order: the resources it CHANGED, each with the member
//! paths it changed there at their published places (`maxStackSize`, `recipes[37].craftingDuration`, `recipes[]` for a
//! list's membership, "" for the whole resource); the resources it created (in creation order: a created resource's GUID
//! is random, `<package>:<ordinal>` names it across runs); the contents it created. After each entrypoint every resource
//! the pass has edited is encoded as the commit encodes it and compared with what it held before that entrypoint ran (its
//! archive value the first time); only a resource whose bytes differ is decoded and walked member by member ([`diff`]).
//! A list whose length changed is aligned by its unchanged first and last elements: its membership and the elements
//! between them are listed, and what earlier packages changed in it moves with the shift ([`Paths::changed`]). A member
//! written back to the value it held is no change: a package is named where it changed something. The record can
//! over-name (a list edited at both ends and in the middle), never miss.
//!
//! **The file.** `<game>/<stem>.kfc.change-journal.json`, beside the record of the pass (`<stem>.kfc.prepared.json`),
//! Kiln's change journal format 2 (Kiln's `schema/sfi/eml-journal/eml-journal-2.schema.json`: dialect `shroudforge`,
//! writer `shroudforge`), bound to the SHA-256 of the `<stem>.kfc` the pass published. The loader removes it before a pass
//! changes the game files ([`forget`]) and writes it after a pass succeeded ([`write`]); an unchanged start keeps it, and
//! a reader takes it only while the live `<stem>.kfc` is the one it is bound to.
//!
//! **Bounds.** At most [`PACKAGE_ENTRIES`] entries (changed, created, contents) per package, [`TOTAL_ENTRIES`] in all
//! (over: `truncated`), [`RESOURCE_PATHS`] paths per changed resource and [`PATH_BYTES`] bytes per path (over: the
//! whole resource, the member holding it), [`JOURNAL_PATH_BYTES`] of paths in all (over: whole resources).
//!
//! **Cost.** At each package's end every resource the pass edited so far is encoded once (the commit encodes each once at
//! the end). Past [`BUDGET`] of that work in one pass, the packages after it are not compared: what changed after it is
//! listed whole in `unattributed` (named as no package's) and the record says `truncated`, so it never misses a change and
//! never makes a pass much slower than before.
use kfc::{
    guid::{ContentHash, ResourceId},
    resource::value::Value,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub const PACKAGE_ENTRIES: usize = 4096;
pub const TOTAL_ENTRIES: usize = 32768;
pub const RESOURCE_PATHS: usize = 16384;
pub const PATH_BYTES: usize = 256;
pub const JOURNAL_PATH_BYTES: usize = 8 << 20;
/// The comparing a pass may spend; past it the rest of the pass is recorded as no package's.
pub const BUDGET: std::time::Duration = std::time::Duration::from_secs(3);

// ---- the member paths ----

/// One step of a member path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Seg {
    Field(Box<str>),
    Index(usize),
    /// A list's membership: always the last step.
    Items,
}

/// A list whose length changed, at `at`: its first `prefix` and last `suffix` elements unchanged, `old` -> `new` elements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shift {
    pub at: Vec<Seg>,
    pub prefix: usize,
    pub suffix: usize,
    pub old: usize,
    pub new: usize,
}

/// What changed between two values of one resource.
#[derive(Debug, Default, PartialEq)]
pub struct Diff {
    /// The changed members, at their places in the new value; one empty path: the whole value.
    pub changed: Vec<Vec<Seg>>,
    pub shifts: Vec<Shift>,
    pub over: bool,
}

/// Two values the same, floats by their bits.
pub fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Float(x), Value::Float(y)) => x.to_bits() == y.to_bits(),
        (Value::Struct(x), Value::Struct(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w))),
        (Value::Variant(x), Value::Variant(y)) => {
            x.type_index == y.type_index && x.value.len() == y.value.len() && x.value.iter().all(|(k, v)| y.value.get(k).is_some_and(|w| same(v, w)))
        }
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(v, w)| same(v, w)),
        _ => a == b,
    }
}

/// The members of `after` that differ from `before`, at most `limit` paths (more: `over`, the whole value).
pub fn diff(before: &Value, after: &Value, limit: usize) -> Diff {
    let mut out = Diff::default();
    let mut path = Vec::new();
    walk(before, after, &mut path, limit, &mut out);
    if out.over {
        out.changed = vec![Vec::new()];
    }
    out
}

fn note(path: &[Seg], limit: usize, out: &mut Diff) {
    if out.changed.len() >= limit {
        out.over = true;
        return;
    }
    out.changed.push(path.to_vec());
}

macro_rules! members {
    ($a:expr, $b:expr, $path:expr, $limit:expr, $out:expr) => {{
        for (k, w) in $b.iter() {
            $path.push(Seg::Field(k.as_str().into()));
            match $a.get(k) {
                Some(v) => walk(v, w, $path, $limit, $out),
                None => note($path, $limit, $out),
            }
            $path.pop();
            if $out.over {
                return;
            }
        }
        for k in $a.keys().filter(|k| !$b.contains_key(*k)) {
            $path.push(Seg::Field(k.as_str().into()));
            note($path, $limit, $out);
            $path.pop();
        }
    }};
}

fn walk(a: &Value, b: &Value, path: &mut Vec<Seg>, limit: usize, out: &mut Diff) {
    if out.over {
        return;
    }
    match (a, b) {
        (Value::Struct(x), Value::Struct(y)) => members!(x, y, path, limit, out),
        (Value::Variant(x), Value::Variant(y)) if x.type_index == y.type_index => members!(x.value, y.value, path, limit, out),
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (i, (v, w)) in x.iter().zip(y).enumerate() {
                path.push(Seg::Index(i));
                walk(v, w, path, limit, out);
                path.pop();
                if out.over {
                    return;
                }
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            let shorter = x.len().min(y.len());
            let prefix = x.iter().zip(y).take_while(|(v, w)| same(v, w)).count();
            let suffix = x.iter().rev().zip(y.iter().rev()).take(shorter - prefix).take_while(|(v, w)| same(v, w)).count();
            path.push(Seg::Items);
            note(path, limit, out);
            path.pop();
            for i in prefix..y.len() - suffix {
                path.push(Seg::Index(i));
                note(path, limit, out);
                path.pop();
                if out.over {
                    return;
                }
            }
            out.shifts.push(Shift { at: path.clone(), prefix, suffix, old: x.len(), new: y.len() });
        }
        _ => {
            if !same(a, b) {
                note(path, limit, out);
            }
        }
    }
}

/// One resource's changed paths per writer (a package's 1-based place in run order), kept at their published places.
#[derive(Debug, Default)]
pub struct Paths {
    entries: Vec<(u32, Vec<Seg>)>,
    whole: Vec<u32>,
}

impl Paths {
    /// `writer`'s change; the earlier writers' paths move with its shifts first.
    pub fn changed(&mut self, writer: u32, d: Diff) {
        for s in &d.shifts {
            let n = s.at.len();
            self.entries.retain_mut(|(_, p)| {
                if p.len() <= n || p[..n] != s.at[..] {
                    return true;
                }
                let Seg::Index(i) = p[n] else { return true };
                if i < s.prefix {
                    true
                } else if i >= s.old - s.suffix {
                    p[n] = Seg::Index(i + s.new - s.old);
                    true
                } else {
                    i < s.new - s.suffix
                }
            });
        }
        if d.over || d.changed.iter().any(Vec::is_empty) {
            return self.whole(writer);
        }
        for p in d.changed {
            if !self.entries.iter().any(|(w, q)| *w == writer && *q == p) {
                self.entries.push((writer, p));
            }
        }
    }

    pub fn whole(&mut self, writer: u32) {
        if !self.whole.contains(&writer) {
            self.whole.push(writer);
        }
    }

    /// `writer`'s paths (`[""]`: the whole resource), sorted, each once; empty: it changed nothing still there.
    pub fn of(&self, writer: u32) -> Vec<String> {
        if self.whole.contains(&writer) {
            return vec![String::new()];
        }
        let mut out: Vec<String> = self.entries.iter().filter(|(w, _)| *w == writer).map(|(_, p)| text(p)).collect();
        out.sort();
        out.dedup();
        out
    }
}

/// A path as the record writes it.
pub fn text(path: &[Seg]) -> String {
    let mut s = String::new();
    for seg in path {
        match seg {
            Seg::Field(k) => {
                if !s.is_empty() {
                    s.push('.');
                }
                s.push_str(k);
            }
            Seg::Index(i) => s.push_str(&format!("[{i}]")),
            Seg::Items => s.push_str("[]"),
        }
    }
    s
}

/// A path cut back to the member holding it when longer than [`PATH_BYTES`].
fn bounded_path(path: &str) -> String {
    if path.len() <= PATH_BYTES {
        return path.to_owned();
    }
    let mut end = PATH_BYTES + 1;
    while !path.is_char_boundary(end) {
        end -= 1;
    }
    path[..end].rfind(['.', '[']).map_or_else(String::new, |at| path[..at].to_owned())
}

// ---- the pass's record ----

#[derive(Debug, Default)]
struct Package {
    id: String,
    version: Option<String>,
    changed: BTreeSet<ResourceId>,
    created: Vec<(ResourceId, bool)>,
    contents: Vec<ContentHash>,
    seen: HashSet<ContentHash>,
}

#[derive(Debug, Default)]
struct State {
    packages: Vec<Package>,
    /// The running package's index (None: none runs).
    current: Option<usize>,
    /// The member paths each package changed, per resource.
    paths: HashMap<ResourceId, Paths>,
    /// Each edited resource's bytes after the last package that changed it.
    before: HashMap<ResourceId, Vec<u8>>,
    /// Every resource created in the pass: its creator's index and ordinal.
    creators: HashMap<ResourceId, (usize, u32)>,
    /// The comparing so far; past BUDGET, `exhausted`: what changed after it is `unattributed`, whole.
    spent: std::time::Duration,
    exhausted: bool,
    unattributed: BTreeSet<ResourceId>,
}

/// The pass's change journal (shared by the loader, which writes it, and the pass, which fills it): reset when an attempt
/// begins, so a retried pass records only its committed attempt.
#[derive(Clone, Default)]
pub struct ChangeJournal(Arc<Mutex<State>>);

impl std::fmt::Debug for ChangeJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChangeJournal")
    }
}

/// What a resource holds now, as the commit would encode it, and before.
pub struct Snapshot {
    pub id: ResourceId,
    /// Its value and bytes now, None: never edited in this pass.
    pub now: Option<(Value, Vec<u8>)>,
    /// Created in this pass (no archive value).
    pub created: bool,
}

impl ChangeJournal {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// An attempt of the pass begins: what an earlier attempt recorded is dropped.
    pub fn reset(&self) {
        *self.state() = State::default();
    }

    /// `id`'s entrypoint begins.
    pub fn begin(&self, id: &str, version: Option<String>) {
        let mut s = self.state();
        s.packages.push(Package { id: id.to_owned(), version, ..Package::default() });
        s.current = Some(s.packages.len() - 1);
    }

    /// The running package created `id` (`explicit`: with its own GUID).
    pub fn created(&self, id: ResourceId, explicit: bool) {
        let mut s = self.state();
        let Some(current) = s.current else { return };
        let package = &mut s.packages[current];
        package.created.push((id, explicit));
        let ordinal = package.created.len() as u32;
        s.creators.insert(id, (current, ordinal));
    }

    /// The running package created the content `hash` (again).
    pub fn content(&self, hash: ContentHash) {
        let mut s = self.state();
        let Some(current) = s.current else { return };
        let package = &mut s.packages[current];
        if package.seen.insert(hash) {
            package.contents.push(hash);
        }
    }

    /// The running package's entrypoint returned: `resources` are the pass's edited resources now (`snapshot`); `archive`
    /// gives a resource's archive bytes and `decode` bytes as its value (None: not available, the whole resource).
    pub fn end(
        &self,
        resources: impl IntoIterator<Item = Snapshot>,
        archive: impl Fn(&ResourceId) -> Option<Vec<u8>>,
        decode: impl Fn(&ResourceId, &[u8]) -> Option<Value>,
    ) {
        let mut s = self.state();
        let Some(current) = s.current.take() else { return };
        if s.exhausted {
            return;
        }
        let started = std::time::Instant::now();
        let writer = current as u32 + 1;
        for r in resources {
            let Some((value, bytes)) = r.now else { continue };
            if r.created && s.creators.get(&r.id).is_some_and(|(by, _)| *by == current) {
                s.before.insert(r.id, bytes); // its creator is named on it whole
                continue;
            }
            let was = match s.before.get(&r.id) {
                Some(was) => Some(was.clone()),
                None if r.created => None,
                None => archive(&r.id),
            };
            if was.as_deref() == Some(&bytes[..]) {
                continue;
            }
            let d = was.as_deref().and_then(|was| decode(&r.id, was)).map(|before| diff(&before, &value, RESOURCE_PATHS));
            let paths = s.paths.entry(r.id).or_default();
            match d {
                Some(d) => paths.changed(writer, d),
                None => paths.whole(writer),
            }
            s.packages[current].changed.insert(r.id);
            s.before.insert(r.id, bytes);
        }
        s.spent += started.elapsed();
        if s.spent > BUDGET {
            s.exhausted = true;
            tracing::warn!(target: "shroudforge::startup", spent_ms = s.spent.as_millis() as u64,
                "The change journal stopped comparing after {} package(s): later changes are recorded as no package's", current + 1);
        }
    }

    /// The pass's end (before its commit), with its edited resources then: when the comparing stopped early, each one
    /// whose bytes differ from what the last compared package left (or from the archive) is no package's, whole.
    pub fn finish(&self, resources: impl IntoIterator<Item = Snapshot>, archive: impl Fn(&ResourceId) -> Option<Vec<u8>>) {
        let mut s = self.state();
        if !s.exhausted {
            return;
        }
        for r in resources {
            let Some((_, bytes)) = r.now else { continue };
            // a resource created after the comparing stopped has no bytes before: no package's, besides its creator
            let was = s.before.get(&r.id).cloned().or_else(|| if r.created { None } else { archive(&r.id) });
            if was.as_deref() != Some(&bytes[..]) {
                s.unattributed.insert(r.id);
            }
        }
    }

    /// Whether the comparing stopped early (the pass calls `finish` then).
    pub fn exhausted(&self) -> bool {
        self.state().exhausted
    }

    /// The record as the file holds it (Kiln's change journal format 2, dialect and writer `shroudforge`), bound to
    /// `generation` (the published `<stem>.kfc`'s SHA-256, lower-case hex). `not_applied`: (id, version, reason) of the
    /// plan's packages the committed attempt did not run.
    pub fn json(&self, stem: &str, generation: &str, attempt: &str, not_applied: &[(String, Option<String>, String)]) -> String {
        let s = self.state();
        let names: Vec<&str> = s.packages.iter().map(|p| p.id.as_str()).collect();
        let mut entries = 0usize;
        let mut path_bytes = 0usize;
        let mut truncated = s.exhausted;
        let key = |id: &ResourceId| format!("\"guid\":\"{}\",\"typeHash\":\"{:08x}\",\"part\":{}", id.guid(), id.type_hash(), id.part_index());
        let mut packages = Vec::new();
        for (i, p) in s.packages.iter().enumerate() {
            let writer = i as u32 + 1;
            let mut cut = false;
            let mut room = |cut: &mut bool| {
                if entries < TOTAL_ENTRIES {
                    entries += 1;
                    true
                } else {
                    *cut = true;
                    false
                }
            };
            let mut created = Vec::new();
            let mut contents = Vec::new();
            let mut changed = Vec::new();
            let mut n = 0usize;
            for (ordinal, (id, explicit)) in p.created.iter().enumerate() {
                if n >= PACKAGE_ENTRIES || !room(&mut cut) {
                    cut = true;
                    break;
                }
                n += 1;
                created.push(format!("{{\"ordinal\":{},{},\"explicit\":{explicit}}}", ordinal + 1, key(id)));
            }
            for hash in &p.contents {
                if n >= PACKAGE_ENTRIES || !room(&mut cut) {
                    cut = true;
                    break;
                }
                n += 1;
                contents.push(format!("{{\"guid\":\"{hash}\",\"size\":{}}}", hash.size()));
            }
            for id in &p.changed {
                let mut paths: Vec<String> = s.paths.get(id).map(|x| x.of(writer)).unwrap_or_else(|| vec![String::new()]);
                if paths.is_empty() {
                    continue; // its change did not reach the published resource
                }
                let set: BTreeSet<String> = paths.iter().map(|p| bounded_path(p)).collect();
                paths = if set.contains("") || set.len() > RESOURCE_PATHS { vec![String::new()] } else { set.into_iter().collect() };
                let cost: usize = paths.iter().map(|p| p.len() + 3).sum();
                if path_bytes + cost > JOURNAL_PATH_BYTES {
                    paths = vec![String::new()];
                }
                if n >= PACKAGE_ENTRIES || !room(&mut cut) {
                    cut = true;
                    break;
                }
                n += 1;
                path_bytes += paths.iter().map(|p| p.len() + 3).sum::<usize>();
                let by = match s.creators.get(id) {
                    Some((creator, ordinal)) if *creator != i => format!("{{\"package\":{},\"ordinal\":{ordinal}}}", text_json(names[*creator])),
                    _ => "null".into(),
                };
                let paths: Vec<String> = paths.iter().map(|p| text_json(p)).collect();
                changed.push(format!("{{{},\"createdBy\":{by},\"paths\":[{}]}}", key(id), paths.join(",")));
            }
            truncated |= cut;
            packages.push(format!(
                "{{\"id\":{},\"version\":{},\"kind\":\"shroudforge\",\"contentSha256\":null,\"settingsSha256\":null,\"truncated\":{cut},\
                 \"changed\":[{}],\"created\":[{}],\"contents\":[{}]}}",
                text_json(&p.id),
                p.version.as_deref().map_or_else(|| "null".into(), text_json),
                changed.join(","),
                created.join(","),
                contents.join(",")
            ));
        }
        let ran: HashSet<&str> = names.iter().copied().collect();
        let mut listed = HashSet::new();
        let not_applied: Vec<String> = not_applied
            .iter()
            .filter(|(id, _, _)| is_sf_id(id) && !ran.contains(id.as_str()) && listed.insert(id.clone()))
            .map(|(id, version, reason)| {
                format!(
                    "{{\"id\":{},\"version\":{},\"kind\":\"shroudforge\",\"contentSha256\":null,\"reason\":{}}}",
                    text_json(id),
                    version.as_deref().map_or_else(|| "null".into(), text_json),
                    text_json(&one_line(reason))
                )
            })
            .collect();
        let unattributed: Vec<String> = s
            .unattributed
            .iter()
            .take(PACKAGE_ENTRIES)
            .map(|id| format!("{{{},\"createdBy\":null,\"paths\":[\"\"]}}", key(id)))
            .collect();
        format!(
            "{{\"format\":2,\"stem\":{},\"generation\":\"{generation}\",\"receiptSha256\":\"{generation}\",\"dialect\":\"shroudforge\",\
             \"attempt\":\"{attempt}\",\"truncated\":{truncated},\"packages\":[{}],\
             \"unattributed\":{{\"truncated\":{},\"changed\":[{}],\"created\":[],\"contents\":[]}},\"writer\":\"shroudforge\",\"notApplied\":[{}]}}",
            text_json(stem),
            packages.join(","),
            s.exhausted,
            unattributed.join(","),
            not_applied.join(",")
        )
    }
}

fn text_json(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// A ShroudForge identifier, as the record names a package: 1 to 80 of A-Z a-z 0-9 . _ -
pub fn is_sf_id(id: &str) -> bool {
    (1..=80).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// One line of at most 512 bytes (control characters as spaces).
fn one_line(text: &str) -> String {
    let s: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let mut s = s.trim().to_owned();
    if s.is_empty() {
        s = "failed".into();
    }
    if s.len() > 512 {
        let mut end = 512;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}

// ---- the file ----

/// `<game>/<stem>.kfc.change-journal.json`.
pub fn journal_path(root: &Path, stem: &str) -> PathBuf {
    root.join(format!("{stem}.kfc.change-journal.json"))
}

/// Removes the journal (none is fine). The loader calls it before a pass changes the game files.
pub fn forget(root: &Path, stem: &str) -> Result<(), String> {
    match fs::remove_file(journal_path(root, stem)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", journal_path(root, stem).display())),
    }
}

/// The SHA-256 (lower-case hex) of the live `<stem>.kfc`: the generation the journal is bound to.
pub fn generation(root: &Path, stem: &str) -> Result<String, String> {
    let path = root.join(format!("{stem}.kfc"));
    let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect())
}

/// Writes the journal of a pass that succeeded (bound to the live `<stem>.kfc`), flushed, then renamed into place.
pub fn write(
    root: &Path,
    stem: &str,
    journal: &ChangeJournal,
    attempt: &str,
    not_applied: &[(String, Option<String>, String)],
) -> Result<(), String> {
    let generation = generation(root, stem)?;
    let text = journal.json(stem, &generation, attempt, not_applied);
    let path = journal_path(root, stem);
    let partial = root.join(format!("{stem}.kfc.change-journal.json.partial"));
    let written = (|| -> std::io::Result<()> {
        use std::io::Write as _;
        let mut f = fs::File::create(&partial)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        drop(f);
        fs::rename(&partial, &path)
    })();
    written.map_err(|e| {
        let _ = fs::remove_file(&partial);
        format!("{}: {e}", path.display())
    })
}

#[cfg(test)]
#[path = "kiln_change_journal_tests.rs"]
mod tests;
