#pragma once

/* Optional host interface of kfc-runtime.dll (Kiln fork, dev.21). It does not change KFC_RUNTIME_ABI_VERSION: a host that
 * does not know it never calls KfcRuntimeSetHost, and kfc-runtime behaves exactly as before.
 *
 * Why it exists. kfc-runtime hooks up to six places in the game's code (game_thread, entity_manager, world_prop_update,
 * world_actor_placement, world_building_dispatch and world_cursor, named by the game profile). On its own it finds each
 * place in the live code and installs a jump only when the live bytes are still the profile's originals, so any DLL that
 * hooked one of those places first makes it refuse (and with it the whole native runtime). A host that already owns the
 * game's code hooks (Kiln's kiln.hooks) can instead find the place in the exe FILE's bytes and put the hook in its own
 * chain, which runs next to or in front of the other DLL's jump.
 *
 * Use. Call KfcRuntimeSetHost(&host) once, before KfcRuntimeInitialize (the hooks are installed by Initialize). It copies
 * the table (the function pointers must stay valid until KfcRuntimeShutdown). A false result means the table was refused
 * (too small, a null function): kfc-runtime keeps hooking the game itself. KfcRuntimeSetHost(NULL) takes the host away
 * again for the next Initialize.
 *
 * With a host, kfc-runtime does not look at the live bytes at all: find_signature decides whether the place exists, and
 * install_hook places the hook. Every callback below is the same function the self-installed trampoline calls.
 */

#include "abi/runtime_api.h"

#if defined(__cplusplus)
extern "C" {
#endif

#define KFC_RUNTIME_HOST_VERSION 1u

/* Which registers a hook passes to its callback (x64, Microsoft calling convention). The callback runs before the
 * instruction(s) at `target`, on the thread that reached the place, and must not change any register.
 *   PLAIN    callback(r15, rbp)
 *   CONTEXT  callback(rcx, 0)
 *   CURSOR   callback(r14 + extra, r13, rbp)     extra = the profile's captureOffset
 */
#define KFC_HOST_HOOK_PLAIN 0u
#define KFC_HOST_HOOK_CONTEXT 1u
#define KFC_HOST_HOOK_CURSOR 2u

/* find_signature results. */
#define KFC_HOST_FOUND 1u        /* exactly one match; *address is its live address */
#define KFC_HOST_NOT_FOUND 0u    /* no match */
#define KFC_HOST_AMBIGUOUS 2u    /* more than one match */
#define KFC_HOST_MISMATCH 3u     /* one match, but the file's bytes there do not start with `expected` */
#define KFC_HOST_UNAVAILABLE 4u  /* the host cannot search now */

typedef struct KfcRuntimeHost {
    uint32_t size;     /* sizeof(KfcRuntimeHost) as the host built it */
    uint32_t version;  /* KFC_RUNTIME_HOST_VERSION */

    /* signature: NUL-terminated profile signature ("48 8B 45 10 ? ?"). The host searches the game's code as the exe
     * file has it, so bytes another module changed in memory do not hide the place. expected/expected_size: the
     * profile's original bytes; the file's bytes at the match must start with them (expected may be NULL). */
    uint32_t(KFC_RUNTIME_CALL* find_signature)(const char* signature, const unsigned char* expected,
                                               size_t expected_size, uintptr_t* address);

    /* Hooks `target` (a find_signature result). name is the profile's hook name ("game_thread"), for the host's log and
     * map. Returns a handle (non-zero) or 0 when the host refused or could not hook it. */
    uint64_t(KFC_RUNTIME_CALL* install_hook)(const char* name, uintptr_t target, uint32_t kind, void* callback,
                                             uintptr_t extra);

    /* Takes the hook away (KfcRuntimeShutdown). A thread already inside the callback finishes it. */
    void(KFC_RUNTIME_CALL* remove_hook)(uint64_t handle);

    /* ---- voxel writes through the host (Kiln 0.2.0-dev.31, optional; the version stays 1) ----
     * A table whose size does not cover both members, or with either NULL, has none: runtime.world.voxel.write then
     * stays kfc-runtime's own. kfc-runtime's own writer finds the game's block writer by reading the game's code from the
     * registered terraforming impact down to it, and refuses (outcome 1, "rejected before changing the world") when any
     * function on that path carries another module's hook, or when the exe file cannot be compared with the loaded code.
     * A host that hooks the game's terraforming (Kiln does, and so do its mods) always has hooks there.
     *
     * voxel_write_ready: non-zero when a write may be attempted now (a world this process runs as its host). Any thread.
     *   It answers runtime.has("runtime.world.voxel.write").
     * voxel_write: KfcRuntimeWorldVoxelWrite's contract. origin and dimensions in cells (each >= 1, at most 65536 cells
     *   in all), `count` values (low byte material, high byte fill), x fastest, then y, then z. True when the world holds
     *   exactly those cells afterwards. *outcome: 0 written (or already so); 1 refused before anything changed; 2 the
     *   write did not read back as asked and the earlier cells were written back and read back; 3 anything else (part of
     *   the write may be in the world). The host logs why it refused or failed. Called from the thread that runs the
     *   mods; the host may block it briefly (to reach the world's own thread). */
    uint32_t(KFC_RUNTIME_CALL* voxel_write_ready)(void);
    bool(KFC_RUNTIME_CALL* voxel_write)(const int32_t* origin, const uint32_t* dimensions, const uint16_t* values,
                                        size_t count, uint32_t* outcome);
} KfcRuntimeHost;

KFC_RUNTIME_API bool KFC_RUNTIME_CALL KfcRuntimeSetHost(const KfcRuntimeHost* host);

#if defined(__cplusplus)
}
#endif
