/* SPDX-License-Identifier: MIT
 * Kiln layer of frostdev-ops/shroudforge: packages a host has off for this start (shroudforge-runtime.dll).
 *
 * A host that runs ShroudForge for a mod profile may have a mod off in that profile while the package's own switch
 * (enabled in its extended.mod.json) is on, for example in a folder the player put into the mods folder himself, which
 * the host never writes into. The host names such packages here, and ShroudForge then treats them as switched off for
 * the rest of the process: the startup asset pass, the DLL Manager and the live runtime leave them out. Nothing on
 * disk changes. A host that never calls this gets ShroudForge's behaviour unchanged.
 *
 * Optional export: look it up with GetProcAddress and treat a missing one as "this runtime takes no skip list"
 * (upstream ShroudForge and fork builds before 0.2.0-dev.31.12 have none); the packages then run as their switch says.
 *
 * Rules for the host:
 *  - ids: the packages' mod.json ids, one per line ('\n'), UTF-16, NUL-terminated. Matched exactly. A package folder
 *    without mod.json (a DLL the DLL Manager loads) is named by its folder name.
 *  - Call before shroudforge_prepare_startup. From its start on the list is fixed and every call returns false.
 *  - Additive: several calls add to one list. An invalid id refuses the whole call and adds nothing.
 *  - True: every named package is off for this start. False: nothing was added, the packages run as before.
 *  - shroudforge_prepare_startup logs one info line per package ("[host-skip] <id>: off in the host's profile ...").
 */
#pragma once
#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

bool shroudforge_host_skip_packages(const uint16_t* ids);

#ifdef __cplusplus
}
#endif
