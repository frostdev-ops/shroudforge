// SPDX-License-Identifier: MIT
//! Kiln layer: whether the game's asset files are the very files Steam installed, by Steam's own manifest.
//!
//! After a game update the working baseline (`<stem>.kfc.bak`, `<stem>.kfc_resources.bak`) is of the previous build
//! and a new one is made from the game's files (`verify_working_baseline.rs`). The archive's own structure shows that
//! those files are a whole archive that no pass of this runtime wrote, not that no other program changed them. Steam
//! does: `steamapps/appmanifest_<app>.acf` names the depot manifests of the installed build (`InstalledDepots`), and
//! Steam keeps each one as `<Steam>/depotcache/<depot>_<manifest>.manifest`, which lists every file of the depot with
//! its size and SHA-1 (the same check as Steam's "Verify integrity of game files"). A Steam client's manifests list
//! the file names in clear (`filenames_encrypted` false), as a real client's do (2026-11-25, kiln-win1, build
//! 26430625: both asset files equal what the manifest lists).
//!
//! Only the header and the resource file are compared; the containers (`_NNN.dat`, gigabytes) are not read.
//! Nothing here writes. A server installed with SteamCMD's `force_install_dir` keeps its `steamapps` folder inside the
//! game folder and its depot manifests with SteamCMD; when no manifest is found, the answer is [`Installed::Unknown`]
//! and the caller keeps its own rule.

use sha1::{Digest, Sha1};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

/// What Steam's manifest says about the game's `<stem>.kfc` and `<stem>.kfc_resources`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// Steam's manifest of the installed build lists both files with exactly these bytes.
    Same { build: Option<String> },
    /// Steam's manifest of the installed build lists them, and a game file is not what it lists (what differs).
    Differs { build: Option<String>, what: String },
    /// No manifest of the installed build was found or read here (why).
    Unknown(String),
}

impl Installed {
    /// "build 26430625", or "the installed build" without a build id.
    pub fn build_words(build: &Option<String>) -> String {
        build.as_ref().map_or_else(|| "the installed build".into(), |build| format!("build {build}"))
    }
}

/// A Steam manifest larger than this is not read (a client's has some 2 MiB).
const MAX_MANIFEST: u64 = 64 * 1024 * 1024;
const PAYLOAD: u32 = 0x71F6_17D0;
const METADATA: u32 = 0x1F48_12BE;
const SIGNATURE: u32 = 0x1B81_B817;
const END: u32 = 0x32C4_15AB;

/// The file of a depot manifest: its size and SHA-1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub size: u64,
    pub sha1: [u8; 20],
}

/// What Steam's manifests say about the asset files of `stem` in the game folder `root`.
pub fn installed_files(root: &Path, stem: &str) -> Installed {
    let app = if stem == "enshrouded_server" { "2278520" } else { "1203620" };
    let Some((steamapps, acf)) = app_manifest(root, app) else {
        return Installed::Unknown(format!("there is no Steam manifest appmanifest_{app}.acf for the game folder"));
    };
    let Some(state) = parse_vdf(&acf).and_then(|root| root.get("AppState").and_then(Vdf::table).cloned()) else {
        return Installed::Unknown(format!("Steam's appmanifest_{app}.acf cannot be read"));
    };
    let build = state.get("buildid").and_then(Vdf::text).map(str::to_owned);
    let depots: Vec<(String, String)> = state
        .get("InstalledDepots")
        .and_then(Vdf::table)
        .map(|depots| {
            depots
                .iter()
                .filter_map(|(depot, entry)| {
                    Some((depot.clone(), entry.table()?.get("manifest")?.text()?.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default();
    if depots.is_empty() {
        return Installed::Unknown(format!("Steam's appmanifest_{app}.acf names no installed depot"));
    }
    let names = [format!("{stem}.kfc"), format!("{stem}.kfc_resources")];
    let (mut why, mut read) = (Vec::new(), false);
    for (depot, manifest) in &depots {
        let file = format!("{depot}_{manifest}.manifest");
        let Some(path) = depot_caches(&steamapps).into_iter().map(|dir| dir.join(&file)).find(|path| path.is_file())
        else {
            why.push(format!("no {file} in Steam's depotcache"));
            continue;
        };
        let listed = match read_manifest(&path, &names) {
            Ok(listed) => listed,
            Err(error) => {
                why.push(format!("{file}: {error}"));
                continue;
            }
        };
        read = true;
        if listed.len() != names.len() {
            continue;
        }
        for (name, listed) in names.iter().zip(&listed) {
            match differs(&root.join(name), listed) {
                Ok(None) => {}
                Ok(Some(what)) => return Installed::Differs { build, what: format!("{name} {what}") },
                Err(error) => return Installed::Unknown(format!("{name} cannot be read: {error}")),
            }
        }
        return Installed::Same { build };
    }
    if read {
        return Installed::Unknown(format!("no installed depot lists {} and {}", names[0], names[1]));
    }
    Installed::Unknown(why.join("; "))
}

/// `appmanifest_<app>.acf` of the library the game folder is in (`<library>/steamapps/common/<game>`), or of a SteamCMD
/// `force_install_dir` (`<game>/steamapps`), with the `steamapps` folder it is in.
fn app_manifest(root: &Path, app: &str) -> Option<(PathBuf, String)> {
    let name = format!("appmanifest_{app}.acf");
    let library = root.parent().and_then(Path::parent).map(Path::to_path_buf);
    [library, Some(root.join("steamapps"))].into_iter().flatten().find_map(|steamapps| {
        let path = steamapps.join(&name);
        fs::symlink_metadata(&path).ok().filter(|metadata| metadata.is_file() && metadata.len() < 1 << 20)?;
        Some((steamapps, fs::read_to_string(&path).ok()?))
    })
}

/// Where Steam keeps the depot manifests: `depotcache` beside the `steamapps` folder of Steam's own library, and, on
/// Windows, in the Steam installation the registry names (a library on another drive has none of its own).
fn depot_caches(steamapps: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Some(library) = steamapps.parent() {
        found.push(library.join("depotcache"));
    }
    #[cfg(windows)]
    if let Some(steam) = steam_install_path() {
        let cache = steam.join("depotcache");
        if !found.contains(&cache) {
            found.push(cache);
        }
    }
    found
}

/// `HKEY_CURRENT_USER\Software\Valve\Steam` `SteamPath`, Steam's own installation folder.
#[cfg(windows)]
fn steam_install_path() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegGetValueW(
            key: isize,
            subkey: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut core::ffi::c_void,
            size: *mut u32,
        ) -> i32;
    }
    const HKEY_CURRENT_USER: isize = 0x8000_0001_u32 as i32 as isize;
    const RRF_RT_REG_SZ: u32 = 0x0000_0002;
    let wide = |text: &str| text.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (subkey, value) = (wide(r"Software\Valve\Steam"), wide("SteamPath"));
    let mut buffer = vec![0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    // SAFETY: the buffer and its size in bytes are passed together; the strings are NUL-terminated.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let length = buffer.iter().position(|&unit| unit == 0).unwrap_or(buffer.len());
    let path = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length]));
    (!path.as_os_str().is_empty()).then_some(path)
}

/// The file at `path` is not the listed one: how (size first, then SHA-1), or None when it is.
fn differs(path: &Path, listed: &Listed) -> std::io::Result<Option<String>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        return Ok(Some("is not a regular file".into()));
    }
    if metadata.len() != listed.size {
        return Ok(Some(format!("has {} bytes, Steam's manifest lists {}", metadata.len(), listed.size)));
    }
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha1::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    Ok((digest.as_slice() != listed.sha1).then(|| "has other bytes than Steam's manifest lists (SHA-1)".into()))
}

/// The entries of `names` in the depot manifest at `path`, in that order (only those found).
pub fn read_manifest(path: &Path, names: &[String]) -> Result<Vec<Listed>, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_MANIFEST {
        return Err("is not a depot manifest Kiln reads".into());
    }
    let data = fs::read(path).map_err(|error| error.to_string())?;
    let mut found: BTreeMap<usize, Listed> = BTreeMap::new();
    let mut payload_seen = false;
    let mut at = 0usize;
    while at + 8 <= data.len() {
        let magic = u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
        let length = u32::from_le_bytes(data[at + 4..at + 8].try_into().unwrap()) as usize;
        at += 8;
        if magic == END {
            break;
        }
        let body = data.get(at..at.checked_add(length).ok_or("a section is too long")?).ok_or("a section is cut off")?;
        at += length;
        match magic {
            PAYLOAD => {
                payload_seen = true;
                for field in Fields(body) {
                    let (number, value) = field?;
                    let (1, Value::Bytes(mapping)) = (number, value) else { continue };
                    let (mut name, mut size, mut sha1) = (None, None, None);
                    for field in Fields(mapping) {
                        match field? {
                            (1, Value::Bytes(text)) => name = std::str::from_utf8(text).ok(),
                            (2, Value::Int(value)) => size = Some(value),
                            (5, Value::Bytes(digest)) => sha1 = <[u8; 20]>::try_from(digest).ok(),
                            _ => {}
                        }
                    }
                    let Some(name) = name else { continue };
                    if let Some(index) = names.iter().position(|wanted| wanted.eq_ignore_ascii_case(name))
                        && let (Some(size), Some(sha1)) = (size, sha1)
                    {
                        found.insert(index, Listed { size, sha1 });
                    }
                }
            }
            METADATA => {
                for field in Fields(body) {
                    if let (4, Value::Int(encrypted)) = field?
                        && encrypted != 0
                    {
                        return Err("its file names are encrypted".into());
                    }
                }
            }
            SIGNATURE => {}
            _ => return Err(format!("an unknown section {magic:#010x}")),
        }
    }
    if !payload_seen {
        return Err("it lists no files".into());
    }
    Ok(found.into_values().collect())
}

enum Value<'a> {
    Int(u64),
    Bytes(&'a [u8]),
    Other,
}

/// The fields of one protobuf message: (number, value).
struct Fields<'a>(&'a [u8]);

impl<'a> Iterator for Fields<'a> {
    type Item = Result<(u64, Value<'a>), String>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_empty() {
            return None;
        }
        let result = (|| {
            let key = varint(&mut self.0)?;
            let value = match key & 7 {
                0 => Value::Int(varint(&mut self.0)?),
                1 => {
                    take(&mut self.0, 8)?;
                    Value::Other
                }
                2 => {
                    let length = usize::try_from(varint(&mut self.0)?).map_err(|_| "a field is too long")?;
                    Value::Bytes(take(&mut self.0, length)?)
                }
                5 => {
                    take(&mut self.0, 4)?;
                    Value::Other
                }
                kind => return Err(format!("a field of unknown kind {kind}")),
            };
            Ok((key >> 3, value))
        })();
        if result.is_err() {
            self.0 = &[];
        }
        Some(result)
    }
}

fn take<'a>(data: &mut &'a [u8], length: usize) -> Result<&'a [u8], String> {
    if data.len() < length {
        return Err("the manifest is cut off".into());
    }
    let (head, rest) = data.split_at(length);
    *data = rest;
    Ok(head)
}

fn varint(data: &mut &[u8]) -> Result<u64, String> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = data.split_first().ok_or("the manifest is cut off")?;
        *data = rest;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err("a number in the manifest is too long".into())
}

/// A value of Valve's text key-value format (`.acf`, `.vdf`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Vdf {
    Text(String),
    Table(BTreeMap<String, Vdf>),
}

impl Vdf {
    fn text(&self) -> Option<&str> {
        match self {
            Vdf::Text(text) => Some(text),
            Vdf::Table(_) => None,
        }
    }
    fn table(&self) -> Option<&BTreeMap<String, Vdf>> {
        match self {
            Vdf::Table(table) => Some(table),
            Vdf::Text(_) => None,
        }
    }
}

enum Token {
    Open,
    Close,
    Text(String),
}

/// The top-level table of a text key-value file, or None when it is not one.
fn parse_vdf(text: &str) -> Option<BTreeMap<String, Vdf>> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                let mut token = String::new();
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            'n' => token.push('\n'),
                            't' => token.push('\t'),
                            other => token.push(other),
                        },
                        other => token.push(other),
                    }
                }
                tokens.push(Token::Text(token));
            }
            '{' => tokens.push(Token::Open),
            '}' => tokens.push(Token::Close),
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    fn table(tokens: &mut std::slice::Iter<'_, Token>, nested: bool) -> Option<BTreeMap<String, Vdf>> {
        let mut out = BTreeMap::new();
        loop {
            let key = match tokens.next() {
                None => return (!nested).then_some(out),
                Some(Token::Close) => return nested.then_some(out),
                Some(Token::Text(key)) => key.clone(),
                Some(Token::Open) => return None,
            };
            let value = match tokens.next()? {
                Token::Open => Vdf::Table(table(tokens, true)?),
                Token::Text(text) => Vdf::Text(text.clone()),
                Token::Close => return None,
            };
            out.insert(key, value);
        }
    }
    table(&mut tokens.iter(), false)
}

/// Kiln layer: Steam's records of a game folder, for tests of the callers (feature `test-fixtures`).
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixture {
    use super::*;

    /// A depot manifest as Steam writes it, listing `files` (name, bytes).
    pub fn manifest(files: &[(&str, &[u8])], encrypted: bool) -> Vec<u8> {
        fn varint(out: &mut Vec<u8>, mut value: u64) {
            loop {
                let byte = (value & 0x7f) as u8;
                value >>= 7;
                if value == 0 {
                    out.push(byte);
                    return;
                }
                out.push(byte | 0x80);
            }
        }
        fn bytes(out: &mut Vec<u8>, field: u64, data: &[u8]) {
            varint(out, field << 3 | 2);
            varint(out, data.len() as u64);
            out.extend_from_slice(data);
        }
        fn int(out: &mut Vec<u8>, field: u64, value: u64) {
            varint(out, field << 3);
            varint(out, value);
        }
        let mut payload = Vec::new();
        for (name, data) in files {
            let mut mapping = Vec::new();
            bytes(&mut mapping, 1, name.as_bytes());
            int(&mut mapping, 2, data.len() as u64);
            int(&mut mapping, 3, 0);
            bytes(&mut mapping, 4, &Sha1::digest(name.as_bytes()));
            bytes(&mut mapping, 5, &Sha1::digest(data));
            // one chunk (field 6), as Steam lists them
            let mut chunk = Vec::new();
            bytes(&mut chunk, 1, &[7; 20]);
            int(&mut chunk, 3, 0);
            bytes(&mut mapping, 6, &chunk);
            bytes(&mut payload, 1, &mapping);
        }
        let mut metadata = Vec::new();
        int(&mut metadata, 1, 1_203_621);
        int(&mut metadata, 2, 1_000_000_000_000_000_001);
        int(&mut metadata, 4, u64::from(encrypted));
        let mut out = Vec::new();
        for (magic, body) in [(PAYLOAD, payload), (METADATA, metadata), (SIGNATURE, Vec::new())] {
            out.extend_from_slice(&magic.to_le_bytes());
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
            out.extend_from_slice(&body);
        }
        out.extend_from_slice(&END.to_le_bytes());
        out
    }

    /// Steam's record of the client in the library whose `steamapps` folder is `steamapps`, at build `build`, and its
    /// depot manifest listing the game folder's files `listed` (name, bytes). The manifest ids are made up: on Windows
    /// the reader also looks in the depotcache of the Steam installation the registry names, which on a player's PC
    /// holds the real ones.
    pub fn install_steam_records(steamapps: &Path, build: &str, listed: &[(&str, &[u8])]) {
        fs::create_dir_all(steamapps).unwrap();
        fs::write(
            steamapps.join("appmanifest_1203620.acf"),
            format!(
                "\"AppState\"\n{{\n\t\"appid\"\t\t\"1203620\"\n\t\"StateFlags\"\t\t\"4\"\n\t\"installdir\"\t\t\"Enshrouded\"\n\t\
                 \"buildid\"\t\t\"{build}\"\n\t\"InstalledDepots\"\n\t{{\n\t\t\"1203621\"\n\t\t{{\n\t\t\t\"manifest\"\t\t\
                 \"1000000000000000001\"\n\t\t\t\"size\"\t\t\"51829059560\"\n\t\t}}\n\t\t\"4951850\"\n\t\t{{\n\t\t\t\
                 \"manifest\"\t\t\"1000000000000000002\"\n\t\t\t\"dlcappid\"\t\t\"4951850\"\n\t\t}}\n\t}}\n\t\"UserConfig\"\n\t{{\n\t\t\
                 \"language\"\t\t\"english\"\n\t}}\n}}\n"
            ),
        )
        .unwrap();
        let cache = steamapps.parent().unwrap().join("depotcache");
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("1203621_1000000000000000001.manifest"), manifest(listed, false)).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{install_steam_records, manifest};
    use super::*;

    struct Library(PathBuf);
    impl Drop for Library {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A Steam library with the game folder `steamapps/common/Enshrouded` holding `files`.
    fn library(files: &[(&str, &[u8])]) -> (Library, PathBuf) {
        let base = std::env::temp_dir().join(format!("sf-steam-files-{}", uuid::Uuid::new_v4()));
        let game = base.join("steamapps/common/Enshrouded");
        fs::create_dir_all(&game).unwrap();
        for (name, data) in files {
            fs::write(game.join(name), data).unwrap();
        }
        (Library(base), game)
    }

    const FILES: [(&str, &[u8]); 3] = [
        ("enshrouded.exe", b"the executable"),
        ("enshrouded.kfc", b"the header Steam installed"),
        ("enshrouded.kfc_resources", b"the resources Steam installed"),
    ];

    #[test]
    fn files_that_steam_lists_are_the_installed_ones() {
        let (library, game) = library(&FILES);
        install_steam_records(&library.0.join("steamapps"), "26430625", &FILES);
        assert_eq!(installed_files(&game, "enshrouded"), Installed::Same { build: Some("26430625".into()) });
    }

    #[test]
    fn a_changed_file_is_not_the_installed_one() {
        let (library, game) = library(&FILES);
        install_steam_records(&library.0.join("steamapps"), "26430625", &FILES);
        // the same length, other bytes: only the hash tells
        fs::write(game.join("enshrouded.kfc"), b"the header a mod rewrote").unwrap();
        fs::write(game.join("enshrouded.kfc"), b"the header Steam installeD").unwrap();
        assert_eq!(
            installed_files(&game, "enshrouded"),
            Installed::Differs {
                build: Some("26430625".into()),
                what: "enshrouded.kfc has other bytes than Steam's manifest lists (SHA-1)".into()
            }
        );
        fs::write(game.join("enshrouded.kfc_resources"), b"longer resources than Steam installed").unwrap();
        fs::write(game.join("enshrouded.kfc"), FILES[1].1).unwrap();
        assert!(matches!(installed_files(&game, "enshrouded"),
            Installed::Differs { what, .. } if what == "enshrouded.kfc_resources has 37 bytes, Steam's manifest lists 29"));
    }

    #[test]
    fn without_steams_records_nothing_is_known() {
        let (library, game) = library(&FILES);
        assert!(matches!(installed_files(&game, "enshrouded"), Installed::Unknown(why) if why.contains("appmanifest_1203620.acf")));
        install_steam_records(&library.0.join("steamapps"), "26430625", &FILES);
        // the server's records are another app's
        assert!(matches!(installed_files(&game, "enshrouded_server"), Installed::Unknown(_)));
        // a manifest without the files, or with encrypted names, proves nothing
        let cache = library.0.join("depotcache/1203621_1000000000000000001.manifest");
        fs::write(&cache, manifest(&FILES[..1], false)).unwrap();
        assert!(matches!(installed_files(&game, "enshrouded"), Installed::Unknown(why) if why.contains("no installed depot lists")));
        fs::write(&cache, manifest(&FILES, true)).unwrap();
        assert!(matches!(installed_files(&game, "enshrouded"), Installed::Unknown(why) if why.contains("encrypted")));
        fs::write(&cache, &manifest(&FILES, false)[..40]).unwrap();
        assert!(matches!(installed_files(&game, "enshrouded"), Installed::Unknown(why) if why.contains("cut off")));
        fs::remove_file(&cache).unwrap();
        assert!(matches!(installed_files(&game, "enshrouded"), Installed::Unknown(why) if why.contains("no 1203621_1000000000000000001.manifest")));
    }

    #[test]
    fn a_steamcmd_install_keeps_its_records_in_the_game_folder() {
        let (_library, game) = library(&FILES);
        install_steam_records(&game.join("steamapps"), "26430625", &FILES);
        assert_eq!(installed_files(&game, "enshrouded"), Installed::Same { build: Some("26430625".into()) });
    }

    #[test]
    fn valves_key_values_read_as_steam_writes_them() {
        let table = parse_vdf("\"AppState\"\n{\n\t\"buildid\"\t\t\"1\"\n\t// a note\n\t\"x\" { \"y\" \"a \\\"b\\\"\" }\n}\n").unwrap();
        let state = table["AppState"].table().unwrap();
        assert_eq!(state["buildid"].text(), Some("1"));
        assert_eq!(state["x"].table().unwrap()["y"].text(), Some("a \"b\""));
        assert!(parse_vdf("\"AppState\" { \"a\" ").is_none());
        assert!(parse_vdf("\"a\" }").is_none());
    }
}
