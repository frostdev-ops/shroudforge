#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
out=${KILN_OUT:?set KILN_OUT to the task build directory}/original-file-policy
mkdir -p "$out"
zydis=${ZYDIS_SOURCE_DIR:?set ZYDIS_SOURCE_DIR to the local pinned Zydis source (no downloads)}
test -f "$zydis/dependencies/zycore/CMakeLists.txt"
cmake -S "$zydis" -B "$out/zydis" -DZYDIS_BUILD_TOOLS=OFF -DZYDIS_BUILD_EXAMPLES=OFF \
    -DZYDIS_BUILD_DOXYGEN=OFF -DZYDIS_BUILD_TESTS=OFF -DZYDIS_FEATURE_ENCODER=OFF \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_C_FLAGS="${HOST_TEST_FLAGS:-}"
cmake --build "$out/zydis" --parallel 3
"${CXX:-c++}" -std=c++20 -Wall -Wextra -Werror ${HOST_TEST_FLAGS:-} -pthread \
    -DZYDIS_STATIC_BUILD -DZYCORE_STATIC_BUILD -I"$zydis/include" -I"$out/zydis" \
    -I"$zydis/dependencies/zycore/include" -I"$out/zydis/zycore" \
    -I"$root/native" "$root/tests/original_file_policy_test.cpp" \
    "$out/zydis/libZydis.a" "$out/zydis/zycore/libZycore.a" -o "$out/test"
"$out/test"
