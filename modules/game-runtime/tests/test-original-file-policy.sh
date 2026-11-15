#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
out=${KILN_OUT:?set KILN_OUT to the task build directory}/original-file-policy
mkdir -p "$out"
"${CXX:-c++}" -std=c++20 -Wall -Wextra -Werror ${HOST_TEST_FLAGS:-} -I"$root/native" "$root/tests/original_file_policy_test.cpp" -o "$out/test"
"$out/test"
