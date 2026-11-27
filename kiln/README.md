# ShroudForge for Kiln (the `kiln` branch of frostdev-ops/shroudforge)

This fork is ShroudForge ([bonsaibauer/shroudforge](https://github.com/bonsaibauer/shroudforge), MIT) with a thin
Kiln layer in this `kiln/` folder. Kiln ships the runtime built from it as a separate component,
`<game>/kiln/plugins/shroudforge/`. Kiln's own MIT plugin `kiln-shroudforge.dll` drives it through the runtime's exported
C functions. With a setting, it drives the player's installed ShroudForge instead. Kiln-side docs:
`docs/kiln/shroudforge-compat.md` in the Kiln repo.

## Layout

| Path | What |
|---|---|
| `kiln/build.sh [OUT]` | builds the SF web UI (vite), `shroudforge-runtime.dll` (`cargo xwin`, crate `shroudforge-modloader`), and `kfc-runtime.dll` (CMake with clang-cl); stages `OUT/shroudforge/` with the profiles, the bundled mods, `version.json`, licences, `NOTICE.txt` and `SOURCE.txt` |
| `kiln/native/CMakeLists.txt` | wraps upstream's `modules/game-runtime/native` for the cross build |
| `kiln/cmake/clang-cl-xwin.cmake` | the toolchain file (a copy of Kiln's) |
| `kiln/sync-upstream.sh` | merges an upstream release branch, rebuilds, tests |
| `kiln/UPSTREAM-VERSION` | the upstream release merged into `kiln` (written into `version.json`) |
| `kiln/UPSTREAM-PATCHES.md` | every edit to an upstream file, one line each |

## The interface Kiln drives

`shroudforge-runtime.dll`: `shroudforge_prepare_startup(const wchar_t* game_root) -> bool`,
`shroudforge_create(root) -> handle`, `shroudforge_update(handle, double dt) -> bool`, `shroudforge_destroy(handle)`.
`kfc-runtime.dll`: `KfcRuntimeAbi`, `KfcRuntimeInitialize`, `KfcRuntimeTick`, `KfcRuntimeShutdown`,
`KfcRuntimeStatus` (`modules/game-runtime/native/abi/runtime_api.h`, ABI 12). These are the functions ShroudForge's own
`winmm.dll` bootstrap calls. If upstream changes them, `kiln-shroudforge` (in Kiln) must follow, and
`sync-upstream.sh` fails loudly.

## Engine hooks through the host (dev.21)

`kfc-runtime.dll` hooks up to six places in the game's code (the profile's `hooks`: game_thread, entity_manager,
world_prop_update, world_actor_placement, world_building_dispatch, world_cursor). Alone it scans the live code and
refuses a place whose bytes another DLL already changed. A host that owns the game's hooks can offer a table before
`KfcRuntimeInitialize`:

```
KfcRuntimeSetHost(const KfcRuntimeHost*)   // modules/game-runtime/native/abi/runtime_host.h, optional export
```

`find_signature` looks in the exe file's bytes and checks the profile's original bytes there, `install_hook` places the
hook (the callback gets the registers of its kind), `remove_hook` takes it away. Kiln's `kiln-shroudforge.dll` does this
with `kiln.hooks` v2. A host that never calls it, such as ShroudForge's own `winmm.dll` bootstrap, gets the behaviour of
upstream unchanged. Test: `cmake --build <dir> --target kfc-runtime-test-engine-hooks`, then run
`bin/kfc-runtime-test-engine-hooks.exe` under Wine (`kiln/native/tests/`).

## Voxel writes through the host (dev.31)

`runtime.world.voxel.write` normally resolves the game's block writer itself: from the registered terraforming impact it
reads the game's code down to the writer, and refuses ("voxel write was rejected before changing the world") when a
function on that path carries a hook or the exe file cannot be compared with the loaded code. Under Kiln that path is
always hooked. A host table that also fills `voxel_write_ready` and `voxel_write` (`abi/runtime_host.h`, inside the
offered `size`) takes the write and its availability instead; SF then never resolves its own writer. Kiln writes through
`kiln.build` on the server world's thread with the same outcome codes (0 written, 1 refused before any change, 2 put
back, 3 uncertain) and logs every refusal. A host without them, or SF's own `winmm.dll` bootstrap, keeps upstream's
writer. Test: `kfc-runtime-test-engine-hooks` (section "voxel writer").

## Keyboard shortcuts through the host (dev.24)

`shroudforge.input.is_keybind_down` and `was_keybind_pressed` take key state from a hidden child process,
`<game>/shroudforge/shroudforge.exe --keyboard-input`. Kiln's bundled component has no `shroudforge.exe`, so without an
installed ShroudForge every mod keybind answered "not held". A host that sees the game window's input can feed the
state itself:

```
shroudforge_input_host_version()                       // 1
shroudforge_input_host_begin()                         // before shroudforge_create; 0 = SF's own input already runs
shroudforge_input_host_raw_key(make_code, flags, vkey) // one RAWKEYBOARD record of WM_INPUT, unchanged
shroudforge_input_host_window_key(wparam, lparam)      // WM_KEYDOWN/UP, WM_SYSKEYDOWN/UP, unchanged
shroudforge_input_host_mouse_button(button, down)      // 1 left, 2 middle, 3 right, 4, 5
shroudforge_input_host_reset()                         // the game window lost the keyboard focus
shroudforge_input_host_end()
```

They are optional exports of `shroudforge-runtime.dll`, declared with the host's rules in `kiln/abi/input_host.h`
(code in `modules/hotkeys/host_input.rs` and `host_keyboard.c`). Keys are translated with SDL's own Windows scan-code
table, so a key gets the `SDL:<n>` code that ShroudForge's settings store. While a host feeds, the runtime starts
neither the helper process nor SDL input. Calls run one at a time, from any thread. Key names for
`shroudforge.input.keybind_label` come from the system keyboard layout. A host that never calls `begin`, such as ShroudForge's own `winmm.dll` bootstrap, gets the behaviour
of upstream unchanged. `kiln/build.sh` fails when the built DLL lacks one of these exports; `kiln/check-contract.py`
compares the Rust definitions with the header. Tests: `cargo xwin test -p shroudforge-hotkeys --target
x86_64-pc-windows-msvc` with Wine as the runner (no game, no window).

## Game updates without the Updater (dev.24)

ShroudForge patches the game archives from working backups, `<stem>.kfc.bak` and `<stem>.kfc_resources.bak`. After a
game update those belong to the previous game version. ShroudForge then has `shroudforge-updater.exe` move them to its
recycle bin and captures new ones from the updated game files. Kiln's bundled component has no Updater, so that step
failed with "ShroudForge Updater is unavailable", asset preparation failed on every start after a game update, and
asset mods stayed off until someone deleted the backups by hand.

In a game folder with no ShroudForge installation (neither `shroudforge/shroudforge-updater.exe` nor
`shroudforge/shroudforge-runtime.dll`), the runtime now moves the outdated pair itself, to
`<game>/shroudforge/backups/outdated-working-baseline/<stem>/<size>-<mtime of the header backup>/`, and goes on as
upstream does: new backups from the current game files, then the patch. Details
(`modules/game-files/set_aside_working_baseline.rs`):

- kiln-eml's seal `<stem>.kfc.bak.integrity` moves with the pair it describes. Left behind, it would no longer match
  the new pair and kiln-eml would refuse the pair. kiln-eml treats an unsealed pair by its own rule: it compares the
  backup with the live resource file and seals the pair when they agree.
- The header backup moves last. A start after an interrupted move still sees an outdated header backup and continues
  in the same folder. The other order could leave an old companion beside a new header backup.
- Only the newest generation is kept.
- The backups, the seal and the folders from `<game>/shroudforge` down must be regular files and folders; a link
  among them is refused before a file moves. The game folder itself and what lies above it are not judged.
- A new backup appears under its name only when its copy is complete (`<name>.partial` until then), so a start that
  ends during the copy of the 200 MiB resource file cannot leave a half backup that a later start restores from.

With a ShroudForge installation in the game folder nothing changes: its Updater and recycle bin are used, and a
missing Updater is still an error. Tests: `cargo test -p shroudforge-game-assets` (host, also under Wine) and
`outdated_working_backups_are_replaced_without_an_updater` in `shroudforge-modloader` (Wine).

## A working baseline that is not the source of the game files (dev.30.7)

Before every startup pass ShroudForge copies `<stem>.kfc.bak` and `<stem>.kfc_resources.bak` over the game files. It
checked only the header backup's version tag and archive format, so a header backup from before a game update that
kept the tag went back over the updated files, and the pass failed with "failed to fill whole buffer" (the old header
pointed past the end of the resource file). Nothing tied the companion to its header either.

`check_working_baseline` (`modules/mod-runtime/startup/verify_working_baseline.rs`) now decides before the pass:

- **Restore** only a whole pair that is the source of the game files. Whole: both backups are the pair recorded in
  `<stem>.kfc.baseline.json` when it was made (size, last-write time and SHA-256 of each,
  `modules/game-files/record_working_baseline.rs`); a pair without a record (made before) must at least hold the
  header backup's resource data in its companion, and is recorded once restored. Source: the live header is the header
  backup, or was built over it (its containers, resource chunks and contents unchanged, only additions after them, as
  every incremental `KFCWriter` writes).
- **Set aside and make anew** when the pair is not the source of the game files (an update or Steam's file check
  wrote them), when it is of another game version (ShroudForge's own rule), or when it is not whole while the game's
  header is its own header: the pair moves away as after a game update (`working_baseline`, the record with it) and
  `capture_working_baseline` copies both game files, then records them. One `startup-assets` warning says why:
  `ShroudForge: its clean copy of the game data (<stem>.kfc.bak, <stem>.kfc_resources.bak) was not used: <why>. It was
  set aside and a new clean copy was made from the game's current files`.
- **Refuse** when no clean copy is left: the game files carry mods built from a pair that is not whole, the game
  files are what the last pass of this runtime left (`prepared_stamp::left_live_header`, read before the pass removes
  that record), or the game's own header points past the end of its resource file. The pass fails with the reason and
  "Verify the game files in Steam (or update the server files with SteamCMD's validate), then start again";
  `recover_failed_startup` refuses the same pair (`restore` makes the check too) and leaves every file as it is.

Tests (Wine): `startup::verify_working_baseline` (5), `baseline_record` (3), and `kiln_baseline_tests` (2): whole
startup passes on a small archive, one after an update that kept the version tag (red before: the old pair went back),
one with a companion too short for its header under modded game files (red before: it was copied over them).

## EML manifests with null optional metadata (dev.30.7)

EML reads its manifest into a type whose `description`, `license`, `icon` and `dependencies[].optional` are optional
(`Option`), so `null` there means "not given". ShroudForge's manifest schema allowed null only for `license` and
`icon`: a package with `"description": null` was refused with "mod: null is not of type string". The schema now takes
null for `description` and `dependencies[].optional` too (`modules/modloader/manifest/manifest.schema.json`; the
parsed manifest already had `Option` fields), and every schema error names the field it is about after the document
(`modules/loader-config/validate_document.rs`): `mod: /description: false is not of type "string"`,
`mod: /dependencies/0/optional: "yes" is not of types "boolean", "null"`. Errors of the whole document (a missing
required field) read as before. `authors`, `capabilities` and `dependencies` stay non-null: EML refuses null there as
well. Tests (Wine): `kiln_manifest_tests` (2, red before), through `ModRegistry::load`.

## Large asset walks keep their Lua values in Lua's registry (dev.30.7)

Every `mlua::Value` that holds a Lua object (a string, table, function or userdata) pins one slot of mlua's auxiliary
stack while it lives, and that stack has about a million slots; past them mlua panics with "cannot create a Lua
reference, out of auxiliary stack space". The values of an asset tree kept one such value for every child a mod
visited (the caches of the game's structs, arrays and variants), a value a mod made one per field and element, and a
resource one for its `data`, so a startup mod that walked a large asset (Forged and Found 0.16.1 on a player's Steam
Deck) ran the whole pass into that panic, and the game started without its asset mods. A walk over 350,000 plain
recipe records, with no mod code, reproduces the exact count (`used 1000194 slots`). Upstream 1.8.1 has the same code.

`StoredLuaValue` (`modules/mod-runtime/lua/stored_lua_value.rs`) keeps such a value in Lua's registry, which has no
such limit, and loads it as a short-lived `mlua::Value` for each operation: the same Lua object every time, so identity
(`rawequal`) and edits made through it hold. nil, booleans and numbers stay plain. The entry is freed when its last
holder drops while the state lives, so a released tree is collected as before; copies share strings and clone userdata
as `try_clone_lua_value` does.

The type metadata `game.types` hands out lives as long as the session, so it is held the same way: each type
(`session/mod.rs` `types`), the caches of `lua/bindings/game/types.rs` (`struct_fields`, `enum_fields`, `attributes`),
and the read-only maps and arrays they are made of (`lua/readonly_collections.rs`: byte keys, `StoredLuaValue` values).
Read whole for the game's 12,658 types (39,163 struct fields, 6,119 enum fields, 10,542 type attributes), it held about
695,000 slots, most of the stack, for the rest of the session.

Tests (host): `lua::resource_values::tests`: a walk over 350,000 game recipes and a mod-made copy of as many (red before
with the player's panic), the references a walk holds (red before: 3002 for 1,000 recipes, 150002 for 50,000; now
under 64 whatever the size, and the registry back to its count once the tree is released), identity, edits, copies,
removal. `lua::bindings::game::kiln_types_tests`: all the metadata of 200 and 2,000 struct types read twice (red
before: 25,437 and 254,037 references; now under 64), and the values, their order and identity (`rawequal`) as before.

## A panic in the startup pass: one plain line (dev.30.7)

The startup pass logged a Rust panic as "Rust panic during startup asset preparation: " and the panic's message, which
named no mod, meant nothing to a player and could run over several lines. `LuaModRunner::run` now runs each package's
entrypoint under `PregameModPanic::name_package` (`modules/mod-runtime/startup/name_failing_mod.rs`): a panic there
unwinds on with the package's name. `report_startup_panic` (`modules/entrypoints/loader/asset_preparation/`, the
loader's panic branch in `embedded.rs`) logs one error line, which the record of the pass keeps as its `error`:
`ShroudForge stopped preparing mod assets because of an internal error while running Forged and Found` (the name a
player sees, the id when it is blank, on one line, at most 80 characters; without "while running" for a panic outside
a package), and the panic's message on an info line of its own for a bug report (`Internal error detail, for a bug
report: ...`, one line, each absolute path in it cut to its last two components, `state/extra.rs:287` or
`Enshrouded\enshrouded.kfc`, so neither the build machine's folders nor a player's reach a log a player shares). A
panic is ShroudForge's own failure, so the pass is not run again without the package; the recovery (saved baseline, the
game starts without the pass) is upstream's. Tests (host): `name_failing_mod`, `report_startup_panic` (the paths:
Windows, Wine, verbatim, Unix, quoted, with spaces, two in one message), and
`kiln_retry_tests::a_panic_in_the_pass_leaves_one_readable_reason_and_the_saved_baseline`.

## One failing startup mod no longer costs the others (dev.29.18)

ShroudForge writes the packages that need the game data changed (a `patch` part, or `export` without a runtime part)
into the archives in one pass before the game starts. Upstream puts the game data back after a failed pass and starts
the game with none of those packages. One package whose own check failed (in a player's report, a clash with another
mod that had changed the localization it looked for) cost every other one.

When the pass fails in one package's own entrypoint, the pass now runs once more without it
(`asset_preparation/retry_without_failed_mod.rs`): from the clean baseline, as every pass starts, with that package
treated as switched off (`ModEnvironment::without`), so the packages that need it drop out too. Once only: no loop, no
search.

- Retried: an error raised by a package's entrypoint while the pass runs it (`LuaModRunner::run` names the package,
  `PregameModError`), such as its own `error(...)` or `assert`, a Lua error, a missing `src/mod.lua`.
- Not retried, upstream's behaviour unchanged: an I/O error (`std::io::Error`) anywhere in the error's chain, read from
  the error itself and so in any language and without an OS code too (`PregameModError::io`); a text with
  `(os error N)` or access denied, for an I/O error the API or the package put into words (the system or the game's
  files); and every failure no package raised: the working baseline (create, restore, set aside), staging, the type
  model, the commit and publication, a replaced `.kfc`.
- A second failure: no startup package loads, as before. The record's `error` is the first failure's.

The record (state.json "assets") gets two optional fields (the schema lists them; readers that do not know them are
unaffected):

```
"skipped": [{"id", "name", "version", "error"}, ...]   the failed package and its error on one line, then the packages
                                                     left out with it ("needs <id>, which failed in the pregame pass")
"retryError": "pregame phase failed: ..."            the second pass's error, when it failed too
```

A pass that ran without a package is recorded `applied` with the configuration's own fingerprint: the next start keeps
the data and the record while nothing changes, and switching the package off (or changing any input) runs the pass
again. Log lines (category `startup-assets`, level W; Kiln copies them into kiln.log as `[sf]` lines):
`ShroudForge: <name> (<id> <version>) failed in the pregame pass (<error>); running the pass once more without it`, then
`...; started the game without it, the other N mods loaded` or `ShroudForge: the pregame pass failed again without
<name> (<error>); the game starts without the mods of the pregame pass`. The in-game Modloader page shows the package
`failed`, the packages left out with it `blocked`, and the pass `partial`. Tests: `cargo xwin test -p shroudforge-api
-p shroudforge-modloader -p shroudforge-package -p shroudforge-modloader-ui --target x86_64-pc-windows-msvc` under Wine
(`kiln_retry_tests.rs` runs whole passes on a small archive pair with made-up packages; `name_failing_mod.rs` raises
I/O errors through a real Lua call).

## Packages the host has off (0.2.0-dev.31.12)

Kiln hosts this runtime for the player's mod profile. A package folder the player put into the mods folder himself is
his: Kiln never writes into it, switches it or moves it. So its own switch (`enabled` in its `extended.mod.json`) could
be on while the profile has the mod off, and ShroudForge ran it (a player's report under Kiln 0.2.0-dev.31.9).
The host can now name such packages before the startup pass:

```
shroudforge_host_skip_packages(const uint16_t* ids)   // mod.json ids (or folder names), one per line; kiln/abi/host_skip.h
```

Every manifest read in this process then has them switched off (`modules/modloader/kiln_host_skip.rs`, checked in
`read_manifest`): the startup asset pass, the DLL Manager's plan and the live runtime leave them out, and nothing on
disk changes. A package folder without mod.json (a DLL the DLL Manager loads by its `native-plugin.ini` or a
compatibility profile) is named by its folder name, and the DLL Manager plans none of its DLLs. The call is additive, refuses an invalid id as a whole, and returns false once
`shroudforge_prepare_startup` has begun, which fixes the list and logs one `[host-skip]` info line per package. A host
that never calls it, such as ShroudForge's own bootstrap, gets the behaviour of upstream unchanged. `kiln/build.sh`
fails when the DLL lacks the export; `kiln/check-contract.py` compares the definition with the header. Tests:
`cargo test -p shroudforge-package kiln_host_skip` (the manifest, the plan, the DLL plan, the sealed list) and
`cargo test -p shroudforge-modloader --lib kiln_host_skip` (the export).

## Textures on every core, and blobs made earlier in the pass (0.2.0-dev.31.13)

**Block-compressed textures on several threads.** A texture-heavy startup pass spent nearly all its time in one
call: EMF Beastmaster's three packs spent 397 of their 401 s of Lua in BC7 encode on one core (about 6 minutes of a
first start). In a pregame pass `image.encode_texture` now cuts a BC1-BC7 texture into strips of whole block rows (at
least 64 blocks each) and encodes each strip with upstream's own `kfc::content::image::encode`; every block depends
only on its own 4x4 pixels, so the output is byte for byte the serial encode
(`modules/mod-runtime/lua/bindings/image/kiln_encode_in_strips.rs`).
- **Where:** `run_with_api` sets the thread count for a pregame pass only (the startup pass of
  `shroudforge_prepare_startup` and the `prepare` command) and puts it back when the pass ends; the in-game runtime
  keeps upstream's serial encode. It logs `Block-compressed textures are encoded on <n> thread(s)`.
- **How many:** the PC's threads, at most 16. `KILN_SF_ENCODE_THREADS=<n>` overrides it (1 = the serial encode; a
  count is used as it is, at most 64).
- **The Windows loader lock:** the pass runs on SF's bootstrap worker (its entry gate) or on the game's main thread in
  Kiln's early step, both outside DllMain. A thread created while some thread holds the loader lock only starts once
  it is free, so the calling thread takes strips from the same counter as its helpers and waits only for strips a
  helper has begun: with no helper running it encodes the whole texture itself. While a helper of an earlier texture
  has not started, no new helpers are made.

**A blob made earlier in the pass.** `game.assets.get_content` answered only blobs listed in the game's own header.
EML (and kiln-eml) also answer blobs created earlier in the same pass. The beasts pack writes the English text table
back as a new blob, the wilds pack's harness script reads it, and under ShroudForge it got nil: the Llama Harness,
Camel Saddle and Mule Harness were not added. It now asks the pass's new contents first (`AppState::has_content`).
`get_all_contents` still lists the header only, as in EML.

Tests: `cargo test -p shroudforge-api --lib kiln_encode` (every BC format against the serial bytes, uneven strips,
helpers that never start, the thread count and its env value) and `cargo test -p shroudforge-modloader --lib
kiln_same_pass` (whole passes: package A creates a blob and B reads it; a texture the pass encodes on several threads
is the serial encode).

## The pass's change journal for Kiln's registry (0.2.0-dev.32)

Kiln names each package of the pregame pass on the registry fields it changed (Kiln's Conflicts view and
`kiln/state/registry.json`), and replays an opted-in package's recorded effect after a game update when its Lua no longer
runs (Kiln's effect replay). Both need to know which package changed what; ShroudForge's own records say only which
packages ran. `modules/mod-runtime/startup/kiln_change_journal.rs` keeps that record, never a value:
- after each package's entrypoint (`lua/mod_runner.rs`) every resource the pass edited so far is encoded as the commit
  encodes it (`ResourceInfo::apply`, no Lua runs) and compared with what it held before that entrypoint (its archive bytes
  the first time); only one whose bytes differ is decoded and walked member by member, and the package is named on the
  members that differ, at their published places (`maxStackSize`, `recipes[37].craftingDuration`, `recipes[]` for a list's
  membership); a list whose length changed is aligned by its unchanged ends, and earlier packages' paths move with it;
- creations (`session/asset_resources.rs` `add_resource_as`: explicit GUID or drawn) and contents (`create_content`) are
  noted per package, in order;
- past 3 s of encoding and comparing in one pass (`BUDGET`, `ChangeJournal::end_with`) the rest of the pass is neither
  encoded nor compared for it and is recorded as no package's (whole resources in `unattributed`, `truncated`), so a large
  modpack's start is never much slower;
- the loader (`entrypoints/loader/asset_preparation/apply_asset_mods.rs`) removes `<stem>.kfc.change-journal.json` with
  the record of the pass before a pass changes the game files, and writes it after a pass succeeded, beside
  `<stem>.kfc.prepared.json`: Kiln's change journal format 2 (Kiln's `schema/sfi/eml-journal/eml-journal-2.schema.json`,
  dialect `shroudforge`, writer `shroudforge`), bound to the SHA-256 of the `<stem>.kfc` it published, with the packages
  the retry ran without as `notApplied`. A start that skips the unchanged pass keeps it;
- each package, run or not applied, carries the digest of its files as `contentSha256` (`package_digest`: the files the
  pass fingerprint hashes, no dot-entry and no `*.log`, framed as Kiln's package digests under the name
  `kiln.shroudforge.package-files.v1`), so Kiln's effect replay matches a recorded effect by id, version and files. It is
  computed only when a pass runs.
Nothing the pass writes changes. Tests: `cargo test -p shroudforge-api --lib kiln_change_journal` (the compare, the
paths, a pass's record from snapshots, the budget, the file) and `cargo test -p shroudforge-modloader --lib
the_pass_writes_its_change_journal` (a whole pass with a failing package, an unchanged start, a fixed one).

## Merging upstream

```
kiln/sync-upstream.sh 1.8.0 --scratch     # trial run in an isolated worktree (retained): conflicts, build, tests
kiln/sync-upstream.sh 1.8.0               # for real, on the kiln branch (uncommitted merge: review, then commit)
```
Resolve conflicts on upstream's side unless the file is in `UPSTREAM-PATCHES.md`. Then bump Kiln's submodule
`third_party/shroudforge` to the new commit.

Trial on 2026-11-07: merging `upstream/1.8.0` into `kiln` (1.7.9) went through with no conflicts, 205 files changed.
The four runtime exports, `KFC_RUNTIME_ABI_VERSION` 12, the kfc-parser submodule and `build_windows_app_icon.rs` are
unchanged in 1.8.0. `kiln` stays on 1.7.9 until the user asks for 1.8.

## Licences

ShroudForge is MIT. `shroudforge-runtime.dll` statically links kfc-parser (Brabb3l, GPL-3.0, a submodule), so that DLL
is distributed under the GPL-3.0. Kiln ships it as a separate component with `LICENSES/`, `NOTICE.txt`, `SOURCE.txt`
and, in each Kiln release, `shroudforge-kiln-<commit>-src.tar.gz` (this tree with its submodules). Files in `kiln/` are
MIT.

Builds require a clean tree and validate initialized submodules against their gitlinks without updating any checkout.
`KILN_SF_ALLOW_DIRTY=1` permits an explicitly marked development build; sync uses it for its uncommitted merge. Release exports carry
`KILN-SOURCE-COMMIT`, `KILN-SOURCE-EPOCH` and `KILN-SOURCE-SUBMODULES.json` from Kiln's recursive source exporter;
no Git repository is needed to build them. UI assets are rebuilt, paths remapped, and staged mtimes normalized.
`sync-upstream.sh` requires a clean `kiln` branch and the official upstream remote, merges the fetched commit directly,
and checks the exact ABI/signatures. Scratch worktrees are retained for review, including conflicts; no force removal,
branch deletion, commit or push is performed by the script.
