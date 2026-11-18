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
`failed` and the pass `partial`. Tests: `cargo xwin test -p shroudforge-modloader -p shroudforge-package -p
shroudforge-modloader-ui --target x86_64-pc-windows-msvc` under Wine (`kiln_retry_tests.rs` runs whole passes on a
small archive pair with made-up packages).

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
