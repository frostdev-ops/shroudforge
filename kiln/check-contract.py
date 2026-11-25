#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Fail if upstream changes the exact unversioned bootstrap signatures Kiln supports."""
import pathlib
import re
root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'modules/entrypoints/loader/embedded.rs').read_text()
for symbol, args, result in [
    ('shroudforge_prepare_startup', 'game: *const u16', 'bool'),
    ('shroudforge_create', 'game: *const u16', '*mut RuntimeHandle'),
    ('shroudforge_update', 'handle: *mut RuntimeHandle, delta_seconds: f64,', 'bool'),
    ('shroudforge_destroy', 'handle: *mut RuntimeHandle', None),
]:
    suffix = r'\s*\{' if result is None else r'\s*->\s*' + re.escape(result) + r'\s*\{'
    pattern = re.escape('#[unsafe(no_mangle)]') + r'\s*pub unsafe extern "C" fn ' + symbol + r'\(\s*' + re.escape(args).replace(r'\ ', r'\s*') + r'\s*\)' + suffix
    if not re.search(pattern, src):
        raise SystemExit('bootstrap signature changed: ' + symbol)

# The host key feed (kiln/abi/input_host.h): the Rust definitions and the C declarations must name the same functions.
feed = (root / 'modules/hotkeys/host_input.rs').read_text()
header = (root / 'kiln/abi/input_host.h').read_text()
for symbol, args, result, c_decl in [
    ('shroudforge_input_host_version', '', 'u32', 'uint32_t shroudforge_input_host_version(void);'),
    ('shroudforge_input_host_begin', '', 'u32', 'uint32_t shroudforge_input_host_begin(void);'),
    ('shroudforge_input_host_raw_key', 'make_code: u32, flags: u32, vkey: u32', None,
     'void shroudforge_input_host_raw_key(uint32_t make_code, uint32_t flags, uint32_t vkey);'),
    ('shroudforge_input_host_window_key', 'wparam: usize, lparam: isize', None,
     'void shroudforge_input_host_window_key(uintptr_t wparam, intptr_t lparam);'),
    ('shroudforge_input_host_mouse_button', 'button: u32, down: u32', None,
     'void shroudforge_input_host_mouse_button(uint32_t button, uint32_t down);'),
    ('shroudforge_input_host_reset', '', None, 'void shroudforge_input_host_reset(void);'),
    ('shroudforge_input_host_end', '', None, 'void shroudforge_input_host_end(void);'),
]:
    suffix = r'\s*\{' if result is None else r'\s*->\s*' + re.escape(result) + r'\s*\{'
    pattern = re.escape('#[unsafe(no_mangle)]') + r'\s*pub extern "C" fn ' + symbol + r'\(\s*' + re.escape(args).replace(r'\ ', r'\s*') + r'\s*\)' + suffix
    if not re.search(pattern, feed):
        raise SystemExit('host key feed signature changed: ' + symbol)
    if c_decl not in header:
        raise SystemExit('kiln/abi/input_host.h does not declare: ' + c_decl)
version = re.search(r'const INPUT_HOST_VERSION: u32 = (\d+);', feed)
if not version or f'#define SHROUDFORGE_INPUT_HOST_VERSION {version.group(1)}u' not in header:
    raise SystemExit('host key feed version differs between host_input.rs and kiln/abi/input_host.h')

# The host's skip list (kiln/abi/host_skip.h): the Rust definition and the C declaration must agree.
skip_header = (root / 'kiln/abi/host_skip.h').read_text()
skip_pattern = re.escape('#[unsafe(no_mangle)]') + r'\s*pub unsafe extern "C" fn shroudforge_host_skip_packages\(\s*ids:\s*\*const u16\s*\)\s*->\s*bool\s*\{'
if not re.search(skip_pattern, src):
    raise SystemExit('host skip list signature changed: shroudforge_host_skip_packages')
if 'bool shroudforge_host_skip_packages(const uint16_t* ids);' not in skip_header:
    raise SystemExit('kiln/abi/host_skip.h does not declare: shroudforge_host_skip_packages')
