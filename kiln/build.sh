#!/usr/bin/env bash
# Kiln layer (MIT): builds ShroudForge's runtime from this fork for Kiln's bundle, cross from Linux.
#   kiln/build.sh [OUT]        OUT default kiln/out (gitignored)
# Produces OUT/shroudforge/ (installed by Kiln as <game>/kiln/plugins/shroudforge/):
#   shroudforge-runtime.dll   SF's runtime (crate shroudforge-modloader, cdylib; links kfc-parser: GPL-3)
#   kfc-runtime.dll           SF's native runtime (C++, MIT)
#   profiles/enshrouded/{client,server}/*.json   game-build profiles (data)
#   mods/<id>/                SF's bundled mods (as in SF's release ZIP)
#   version.json              {"version": <kiln/UPSTREAM-VERSION>, "build": "kiln.<commit>"}
#   LICENSES/, NOTICE.txt, SOURCE.txt   GPL-3 + MIT notices, the exact source commit
# Needs: cargo-xwin, clang-cl/lld-link/llvm-rc, cmake, ninja, node/npm (SF's in-game Modloader page is built with vite
# and embedded into the runtime). Environment: CARGO_TARGET_DIR (default OUT/cargo), NICE (default 10).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
out=$(mkdir -p "${1:-$here/out}" && cd "${1:-$here/out}" && pwd)
[[ $out != / && $out != "$root" && $root != "$out/"* ]] || { echo "unsafe output directory" >&2; exit 2; }
nice=${NICE:-10}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$out/cargo}
cd "$root"
# Release sources are git archives, not checkouts. Never inherit a parent repository's identity.
if [[ -f KILN-SOURCE-COMMIT ]]; then
  read -r source_repo commit < KILN-SOURCE-COMMIT
  [[ $source_repo == frostdev-ops/shroudforge && $commit =~ ^[0-9a-f]{40}$ ]] || { echo "invalid source marker" >&2; exit 1; }
  epoch=$(<KILN-SOURCE-EPOCH)
  submodules=$(<KILN-SOURCE-SUBMODULES.json)
  dirty=""
else
  [[ $(git rev-parse --show-toplevel) == "$root" ]] || { echo "not a fork checkout or source export" >&2; exit 1; }
  commit=$(git rev-parse HEAD)
  epoch=$(git log -1 --format=%ct HEAD)
  submodules=$(git submodule status --recursive)
  [[ -n $submodules && ! $submodules =~ (^|$'\n')[-+U] ]] || { echo "submodules are missing or differ from the pinned commits" >&2; exit 1; }
  dirty=$(git status --porcelain --untracked-files=all --ignore-submodules=none)
  [[ -z $dirty || ${KILN_SF_ALLOW_DIRTY:-0} == 1 ]] || { echo "working tree must be clean (KILN_SF_ALLOW_DIRTY=1 for an explicit development build)" >&2; exit 1; }
fi
[[ $epoch =~ ^[0-9]+$ ]] || { echo "invalid source epoch" >&2; exit 1; }
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$epoch}
for required in kfc-parser/Cargo.toml modules/game-runtime/native/json/LICENSE.MIT modules/hotkeys/SDL/LICENSE.txt; do
  [[ -f $required ]] || { echo "missing pinned submodule content: $required" >&2; exit 1; }
done
# Build from the current sources every time; a cached HTML file can belong to a different commit.
flags=${CARGO_ENCODED_RUSTFLAGS:-}
export CARGO_ENCODED_RUSTFLAGS=${flags:+$flags$'\x1f'}-Ctarget-feature=+crt-static
export CARGO_ENCODED_RUSTFLAGS=$CARGO_ENCODED_RUSTFLAGS$'\x1f'"--remap-path-prefix=$root=/shroudforge"$'\x1f'"--remap-path-prefix=$out=/build/shroudforge"$'\x1f'"-Clink-arg=/Brepro"



# 1. SF's web UI (embedded with include_str! into the runtime)
(cd modules/modloader/ui && nice -n "$nice" npm ci --no-audit --no-fund >/dev/null && nice -n "$nice" npm run build >/dev/null)

# 2. shroudforge-runtime.dll (SHROUDFORGE_NO_APP_ICON: winres cannot run rc.exe on Linux; the DLL needs no icon)
SHROUDFORGE_NO_APP_ICON=1 nice -n "$nice" cargo xwin build --release --locked -p shroudforge-modloader --lib \
  --target x86_64-pc-windows-msvc
dll=$CARGO_TARGET_DIR/x86_64-pc-windows-msvc/release/shroudforge_modloader.dll
[[ -f $dll ]] || { echo "shroudforge_modloader.dll not built" >&2; exit 1; }
# The functions kiln-shroudforge.dll looks up must be in the export table (the host key feed lives in a dependency
# crate, kiln/abi/input_host.h).
exports=$(llvm-readobj --coff-exports "$dll")
for sym in shroudforge_prepare_startup shroudforge_create shroudforge_update shroudforge_destroy \
    shroudforge_input_host_version shroudforge_input_host_begin shroudforge_input_host_raw_key \
    shroudforge_input_host_window_key shroudforge_input_host_mouse_button shroudforge_input_host_reset \
    shroudforge_input_host_end shroudforge_host_skip_packages; do
  grep -qx "  Name: $sym" <<<"$exports" || { echo "shroudforge-runtime.dll does not export $sym" >&2; exit 1; }
done

# 3. kfc-runtime.dll
nb=$out/native
nice -n "$nice" cmake -S "$here/native" -B "$nb" -G Ninja -DCMAKE_TOOLCHAIN_FILE="$here/cmake/clang-cl-xwin.cmake" \
  -DCMAKE_BUILD_TYPE=Release -Wno-dev >/dev/null
nice -n "$nice" cmake --build "$nb" --target kfc-runtime >/dev/null

# 4. stage
stage=$(mktemp -d "$out/.shroudforge.XXXXXX")
trap 'rm -rf -- "$stage"' EXIT
mkdir -p "$stage/LICENSES"
cp "$dll" "$stage/shroudforge-runtime.dll"
cp "$nb/bin/kfc-runtime.dll" "$stage/kfc-runtime.dll"
cp -r modules/game-runtime/profiles "$stage/profiles"
rm -f "$stage/profiles/README.md"
if [[ -d mods ]]; then
  mkdir -p "$stage/mods"
  for m in mods/*/; do if [[ -f $m/mod.json ]]; then cp -r "$m" "$stage/mods/"; fi; done
fi
cp LICENSE "$stage/LICENSES/ShroudForge-MIT.txt"
cp kfc-parser/LICENSE "$stage/LICENSES/kfc-parser-GPL-3.0.txt"
cp modules/game-runtime/native/json/LICENSE.MIT "$stage/LICENSES/nlohmann-json-MIT.txt"
cp modules/hotkeys/SDL/LICENSE.txt "$stage/LICENSES/SDL3-zlib.txt"
zyd=$(find "$nb" -path '*zydis-src/LICENSE' -print -quit)
zyc=$(find "$nb" -path '*zycore/LICENSE' -print -quit)
[[ -f $zyd && -f $zyc ]] || { echo "missing zydis/zycore licences" >&2; exit 1; }
cp "$zyd" "$stage/LICENSES/zydis-MIT.txt"
cp "$zyc" "$stage/LICENSES/zycore-MIT.txt"
sfver=$(tr -d "[:space:]" < "$here/UPSTREAM-VERSION")
printf '{"version":"%s","build":"kiln.%s","host":"kiln"}\n' "$sfver" "${commit:0:7}" > "$stage/version.json"
cat > "$stage/SOURCE.txt" <<EOF
ShroudForge runtime for Kiln, built from https://github.com/frostdev-ops/shroudforge
commit $commit${dirty:+ (with uncommitted changes)}
upstream https://github.com/bonsaibauer/shroudforge ($sfver)
submodules:
$submodules
The complete corresponding source, submodules included, ships with each Kiln release as
shroudforge-kiln-<commit>-src.tar.gz.
EOF
cat > "$stage/NOTICE.txt" <<'EOF'
This folder is a separate program shipped with Kiln: ShroudForge's runtime (MIT, bonsaibauer and contributors), built
from the frostdev-ops/shroudforge fork. shroudforge-runtime.dll statically links kfc-parser (Brabb3l, GPL-3.0), so
shroudforge-runtime.dll is distributed under the GNU General Public License version 3 (LICENSES/kfc-parser-GPL-3.0.txt).
kfc-runtime.dll is ShroudForge's own MIT code with zydis/zycore (MIT) and nlohmann/json (MIT). The SDL3 keyboard tables
in shroudforge-runtime.dll are zlib-licensed. Source: SOURCE.txt. Kiln itself (kiln-shroudforge.dll and everything
in the MIT tree) is MIT and only loads these files through their exported C functions.
EOF
# Normalize payload metadata and replace only a complete component. A failed build never replaces the old output.
find "$stage" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
rm -rf -- "$out/shroudforge"
mv -- "$stage" "$out/shroudforge"
trap - EXIT
stage=$out/shroudforge
echo "staged $stage ($(du -sh "$stage" | cut -f1)) from $commit"
