// SPDX-License-Identifier: MIT
// Kiln layer: tests of kfc-runtime's engine-hook installer (modules/game-runtime/native/engine-hooks/install_engine_hooks.cpp),
// both ways: SF hooking the game itself (no host) and through a host (abi/runtime_host.h, what Kiln offers).
// Runs under Wine, no game: the "game" is six small functions in this exe's own `.game` section, hand-assembled so the
// bytes the profile names are known. Everything else the installer touches is stubbed below.
//
//   cmake --build <dir> --target kfc-runtime-test-engine-hooks  &&  wine <dir>/bin/kfc-runtime-test-engine-hooks.exe
#include <windows.h>

#include <atomic>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <string>
#include <tuple>
#include <vector>

#include "abi/runtime_host.h"
#include "engine-context/observe_entity_manager.h"
#include "engine-context/world_context.h"
#include "engine-hooks/install_engine_hooks.h"
#include "game-thread/game_thread_state.h"
#include "patches/set_runtime_patch.h"
#include "player-input/sample_world_cursor.h"
#include "profiles/game_profile.h"

// ---------------------------------------------------------------------------------------------------------------------
// Stubs for the rest of kfc-runtime: each callback records that it ran and the arguments it got.
namespace {
struct Seen {
    std::atomic<unsigned> calls{0};
    std::atomic<std::uintptr_t> a{0}, b{0}, c{0};
};
Seen g_seen[6];  // game_thread, entity_manager, world_prop_update, world_actor_placement, building_dispatch, cursor
void note(int i, const void* a, const void* b = nullptr, const void* c = nullptr) {
    g_seen[i].a = reinterpret_cast<std::uintptr_t>(a);
    g_seen[i].b = reinterpret_cast<std::uintptr_t>(b);
    g_seen[i].c = reinterpret_cast<std::uintptr_t>(c);
    ++g_seen[i].calls;
}
bool g_cursor_ready = false;
}  // namespace

namespace GameThreadImplementation {
std::atomic<bool> accepting{};
std::atomic<bool> entity_context_hooks_ready{};
std::atomic<std::uint32_t> engine_thread{};
std::atomic<bool> command_observed{};
std::string initialization_error;
std::atomic<std::uintptr_t> entity_manager{};
std::atomic<std::uint64_t> manager_changes{}, last_manager_observation_ms{};
void CancelPendingOperations() {}
void __cdecl drain(void* a, void* b) { note(0, a, b); }
void __cdecl capture_entity_manager(void* a, void* b) { note(1, a, b); }
}  // namespace GameThreadImplementation
namespace WorldRuntime {
void ResetContext() {}
void OnPropUpdate(void* a, void* b) { note(2, a, b); }
void OnActorPlacement(void* a, void* b) { note(3, a, b); }
void OnBuildingDispatch(void* a) { note(4, a); }
void OnCursorUpdate(void* a, void* b, void* c) { note(5, a, b, c); }
void SetCursorHookReady(bool ready) { g_cursor_ready = ready; }
}  // namespace WorldRuntime
namespace PatchRuntime {
bool Initialize() { return true; }
void Shutdown() {}
}  // namespace PatchRuntime

// ---------------------------------------------------------------------------------------------------------------------
// The game: game_fN(x) returns x + N*0x1000, f4 and f5 are the shapes of the building-dispatch entry and the cursor site.
//   f0..f3  mov rax,rcx (3) | add rax,imm32 (6) | ret            hook place = the first 9 bytes
//   f4      mov [rsp+10h],rbx (5) | mov rax,rcx | add rax,imm | ret      hook place = the first 5 bytes (a function entry)
//   f5      mov rax,imm32 (7) | add rax,rcx (3) | ret             hook place = the first 7 bytes
// game_call(fn, x, r15, rbp, r14, r13) calls fn(x) with those registers set (like the real places have them).
#define GAME_FN(n)                                                                                                     \
    ".balign 32\n.globl game_f" #n "\n.def game_f" #n "; .scl 2; .type 32; .endef\ngame_f" #n ":\n"
extern "C" {
std::uint64_t game_f0(std::uint64_t);
std::uint64_t game_f1(std::uint64_t);
std::uint64_t game_f2(std::uint64_t);
std::uint64_t game_f3(std::uint64_t);
std::uint64_t game_f4(std::uint64_t);
std::uint64_t game_f5(std::uint64_t);
std::uint64_t game_call(void* fn, std::uint64_t x, std::uint64_t r15, std::uint64_t rbp, std::uint64_t r14,
                        std::uint64_t r13);
}
__asm__(".intel_syntax noprefix\n.section .game,\"xr\"\n"
        GAME_FN(0) "mov rax, rcx\n add rax, 0x1000\n ret\n"
        GAME_FN(1) "mov rax, rcx\n add rax, 0x2000\n ret\n"
        GAME_FN(2) "mov rax, rcx\n add rax, 0x3000\n ret\n"
        GAME_FN(3) "mov rax, rcx\n add rax, 0x4000\n ret\n"
        GAME_FN(4) "mov [rsp+0x10], rbx\n mov rax, rcx\n add rax, 0x5000\n ret\n"
        GAME_FN(5) "mov rax, 0x6000\n add rax, rcx\n ret\n"
        ".balign 32\n.globl game_call\n.def game_call; .scl 2; .type 32; .endef\ngame_call:\n"
        "mov r10, [rsp+0x28]\n mov r11, [rsp+0x30]\n"
        "push rbp\n push r15\n push r14\n push r13\n push rbx\n sub rsp, 0x20\n"
        "mov r14, r10\n mov r13, r11\n mov r15, r8\n mov rbp, r9\n mov rax, rcx\n mov rcx, rdx\n"
        "call rax\n"
        "add rsp, 0x20\n pop rbx\n pop r13\n pop r14\n pop r15\n pop rbp\n ret\n"
        ".balign 32\n int3\n"
        ".text\n");

namespace {
int g_failures = 0;
#define CHECK(cond, ...)                                                                                              \
    do {                                                                                                               \
        if (cond) {                                                                                                    \
            std::printf("PASS %s\n", #cond);                                                                           \
        } else {                                                                                                       \
            std::printf("FAIL %s  (line %d) ", #cond, __LINE__);                                                       \
            std::printf("" __VA_ARGS__);                                                                               \
            std::printf("\n");                                                                                         \
            ++g_failures;                                                                                              \
        }                                                                                                              \
    } while (0)

struct Place {
    const char* name;
    void* fn;
    std::size_t original;  // bytes the profile names
    std::size_t total;     // bytes of the function up to and including ret
    bool plain, context, cursor;
};
const Place kPlaces[6] = {
    {"game_thread", reinterpret_cast<void*>(&game_f0), 9, 10, true, false, false},
    {"entity_manager", reinterpret_cast<void*>(&game_f1), 9, 10, true, false, false},
    {"world_prop_update", reinterpret_cast<void*>(&game_f2), 9, 10, true, false, false},
    {"world_actor_placement", reinterpret_cast<void*>(&game_f3), 9, 10, true, false, false},
    {"world_building_dispatch", reinterpret_cast<void*>(&game_f4), 5, 15, false, true, false},
    {"world_cursor", reinterpret_cast<void*>(&game_f5), 7, 11, false, false, true},
};

std::string hex(const std::uint8_t* p, std::size_t n) {
    std::string s;
    char b[4];
    for (std::size_t i = 0; i < n; ++i) {
        std::snprintf(b, sizeof b, "%02X ", p[i]);
        s += b;
    }
    if (!s.empty()) s.pop_back();
    return s;
}

// The pristine bytes of the game section: what "the exe file" has.
std::vector<std::uint8_t> g_file;
std::uintptr_t g_section = 0;

std::uint8_t* live(const Place& p) { return static_cast<std::uint8_t*>(p.fn); }
const std::uint8_t* pristine(const Place& p) { return g_file.data() + (live(p) - reinterpret_cast<std::uint8_t*>(g_section)); }

void use_profile() {
    namespace P = KfcRuntimeCompatibility::EnshroudedClient;
    const auto* dos = reinterpret_cast<const IMAGE_DOS_HEADER*>(GetModuleHandleW(nullptr));
    const auto* nt = reinterpret_cast<const IMAGE_NT_HEADERS64*>(reinterpret_cast<const std::uint8_t*>(dos) + dos->e_lfanew);
    P::image_timestamp = nt->FileHeader.TimeDateStamp;
    P::image_size = nt->OptionalHeader.SizeOfImage;
    auto set = [&](std::size_t i, std::string& sig, std::vector<std::uint8_t>& orig) {
        sig = hex(pristine(kPlaces[i]), kPlaces[i].total);
        orig.assign(pristine(kPlaces[i]), pristine(kPlaces[i]) + kPlaces[i].original);
    };
    set(0, P::game_thread_signature, P::game_thread_original);
    set(1, P::entity_manager_signature, P::entity_manager_original);
    set(2, P::world_prop_update_signature, P::world_prop_update_original);
    set(3, P::world_actor_placement_signature, P::world_actor_placement_original);
    set(4, P::world_building_dispatch_signature, P::world_building_dispatch_original);
    set(5, P::world_cursor_signature, P::world_cursor_original);
    P::world_cursor_capture_offset = 0x270;
}

void reset_seen() {
    for (auto& s : g_seen) s.calls = 0, s.a = s.b = s.c = 0;
}

bool bytes_are_file(const Place& p) { return !std::memcmp(live(p), pristine(p), p.total); }

// Runs every place once with distinct register values; checks the game's own result and what each callback got.
void run_all_and_check(const char* what) {
    reset_seen();
    const std::uint64_t r15 = 0x1500, rbp = 0xbb00, r14 = 0x1400, r13 = 0x1300;
    std::printf("-- %s\n", what);
    for (std::size_t i = 0; i < 6; ++i) {
        const std::uint64_t x = 7 + i;
        const std::uint64_t got = game_call(kPlaces[i].fn, x, r15 + i, rbp + i, r14 + i, r13 + i);
        const std::uint64_t want = i == 5 ? 0x6000 + x : x + 0x1000 * (i + 1);
        CHECK(got == want, "%s: result %llx want %llx", kPlaces[i].name, (unsigned long long)got, (unsigned long long)want);
    }
    for (std::size_t i = 0; i < 6; ++i) CHECK(g_seen[i].calls == 1, "%s ran %u times", kPlaces[i].name, g_seen[i].calls.load());
    // plain: (r15, rbp); context: (rcx, 0); cursor: (r14 + captureOffset, r13, rbp)
    for (std::size_t i = 0; i < 4; ++i) {
        CHECK(g_seen[i].a == r15 + i && g_seen[i].b == rbp + i, "%s got (%llx, %llx)", kPlaces[i].name,
              (unsigned long long)g_seen[i].a.load(), (unsigned long long)g_seen[i].b.load());
    }
    CHECK(g_seen[4].a == 7 + 4, "dispatch got rcx %llx", (unsigned long long)g_seen[4].a.load());
    CHECK(g_seen[5].a == r14 + 5 + 0x270 && g_seen[5].b == r13 + 5 && g_seen[5].c == rbp + 5,
          "cursor got (%llx, %llx, %llx)", (unsigned long long)g_seen[5].a.load(), (unsigned long long)g_seen[5].b.load(),
          (unsigned long long)g_seen[5].c.load());
}

// ---------------------------------------------------------------------------------------------------------------------
// A fake host: searches the pristine copy ("the exe file"), records what it is asked to hook, can be told to fail.
struct FakeHost {
    std::vector<std::string> names;
    std::vector<std::uint32_t> kinds;
    std::vector<std::uintptr_t> extras, targets;
    std::vector<void*> callbacks;
    std::vector<std::uint64_t> removed;
    std::uint32_t next_handle = 100;
    const char* refuse_name = nullptr;      // install_hook returns 0 for this name
    const char* ambiguous_name = nullptr;   // find_signature says AMBIGUOUS for the signature of this place
    const char* unavailable = nullptr;      // find_signature says UNAVAILABLE
    std::vector<std::string> searched;
    void clear() { *this = FakeHost{}; }
};
FakeHost g_host;

int place_of_signature(const char* sig) {
    for (int i = 0; i < 6; ++i)
        if (hex(pristine(kPlaces[i]), kPlaces[i].total) == sig) return i;
    return -1;
}

std::uint32_t KFC_RUNTIME_CALL fake_find(const char* signature, const unsigned char* expected, size_t expected_size,
                                         uintptr_t* address) {
    g_host.searched.push_back(signature);
    if (g_host.unavailable) return KFC_HOST_UNAVAILABLE;
    const int i = place_of_signature(signature);
    if (i < 0) return KFC_HOST_NOT_FOUND;
    if (g_host.ambiguous_name && !std::strcmp(g_host.ambiguous_name, kPlaces[i].name)) return KFC_HOST_AMBIGUOUS;
    if (expected && std::memcmp(pristine(kPlaces[i]), expected, expected_size)) return KFC_HOST_MISMATCH;
    *address = reinterpret_cast<std::uintptr_t>(kPlaces[i].fn);
    return KFC_HOST_FOUND;
}
std::uint64_t KFC_RUNTIME_CALL fake_install(const char* name, uintptr_t target, uint32_t kind, void* callback,
                                            uintptr_t extra) {
    if (g_host.refuse_name && !std::strcmp(g_host.refuse_name, name)) return 0;
    g_host.names.push_back(name);
    g_host.targets.push_back(target);
    g_host.kinds.push_back(kind);
    g_host.callbacks.push_back(callback);
    g_host.extras.push_back(extra);
    return g_host.next_handle++;
}
void KFC_RUNTIME_CALL fake_remove(std::uint64_t handle) { g_host.removed.push_back(handle); }

// The host's voxel writer (runtime_host.h voxel_write_ready / voxel_write): what it got and what it answers.
struct FakeVoxels {
    std::uint32_t ready = 1, outcome = 0, calls = 0;
    bool result = true;
    std::int32_t origin[3]{};
    std::uint32_t dims[3]{};
    std::vector<std::uint16_t> values;
} g_voxels;
std::uint32_t KFC_RUNTIME_CALL fake_voxel_ready() { return g_voxels.ready; }
bool KFC_RUNTIME_CALL fake_voxel_write(const std::int32_t* origin, const std::uint32_t* dims, const std::uint16_t* values,
                                       std::size_t count, std::uint32_t* outcome) {
    ++g_voxels.calls;
    std::memcpy(g_voxels.origin, origin, sizeof g_voxels.origin);
    std::memcpy(g_voxels.dims, dims, sizeof g_voxels.dims);
    g_voxels.values.assign(values, values + count);
    *outcome = g_voxels.outcome;
    return g_voxels.result;
}

KfcRuntimeHost make_host() {
    KfcRuntimeHost h{};
    h.size = sizeof h;
    h.version = KFC_RUNTIME_HOST_VERSION;
    h.find_signature = &fake_find;
    h.install_hook = &fake_install;
    h.remove_hook = &fake_remove;
    return h;
}

// Another module's hook the way Enshwick/XHL do it: E9 rel32 at the function, a relay that runs the displaced
// instructions and jumps back past them. (`func` + 9 is the first instruction boundary after 5 bytes in f1.)
std::uint8_t* near_alloc(const void* where, std::size_t n) {
    const std::uintptr_t base = reinterpret_cast<std::uintptr_t>(where) & ~std::uintptr_t{0xFFFF};
    for (std::uintptr_t d = 0x10000; d < 0x70000000; d += 0x10000)
        for (int dir = 0; dir < 2; ++dir) {
            void* p = VirtualAlloc(reinterpret_cast<void*>(dir ? base + d : base - d), n, MEM_COMMIT | MEM_RESERVE,
                                   PAGE_EXECUTE_READWRITE);
            if (p) return static_cast<std::uint8_t*>(p);
        }
    return nullptr;
}
void foreign_hook(const Place& p) {
    std::uint8_t* func = live(p);
    std::uint8_t* relay = near_alloc(func, 0x100);
    // relay: the displaced instructions (mov rax,rcx; add rax,imm32 = 9 bytes), then jmp func+9
    std::memcpy(relay, func, 9);
    relay[9] = 0xE9;
    const std::int32_t back = static_cast<std::int32_t>((func + 9) - (relay + 10 + 4));
    std::memcpy(relay + 10, &back, 4);
    DWORD old;
    VirtualProtect(func, 16, PAGE_EXECUTE_READWRITE, &old);
    func[0] = 0xE9;
    const std::int32_t rel = static_cast<std::int32_t>(relay - (func + 5));
    std::memcpy(func + 1, &rel, 4);
    std::memset(func + 5, 0x90, 4);  // the rest of the displaced instructions
    FlushInstructionCache(GetCurrentProcess(), func, 16);
}

}  // namespace

int main() {
    // Locate the .game section and keep its pristine bytes.
    const auto* base = reinterpret_cast<const std::uint8_t*>(GetModuleHandleW(nullptr));
    const auto* dos = reinterpret_cast<const IMAGE_DOS_HEADER*>(base);
    const auto* nt = reinterpret_cast<const IMAGE_NT_HEADERS64*>(base + dos->e_lfanew);
    const auto* sec = IMAGE_FIRST_SECTION(nt);
    std::size_t section_size = 0;
    for (WORD i = 0; i < nt->FileHeader.NumberOfSections; ++i)
        if (!std::memcmp(sec[i].Name, ".game", 5)) {
            g_section = reinterpret_cast<std::uintptr_t>(base + sec[i].VirtualAddress);
            section_size = sec[i].Misc.VirtualSize;
        }
    if (!g_section) { std::puts("FAIL no .game section"); return 1; }
    g_file.assign(reinterpret_cast<const std::uint8_t*>(g_section), reinterpret_cast<const std::uint8_t*>(g_section) + section_size);
    use_profile();

    // ---- 1. no host: SF hooks the game itself (behaviour as before this change) ----
    std::puts("== no host: the installer's own trampolines");
    CHECK(EngineHooks::Install(), "install failed: %s", GameThreadImplementation::initialization_error.c_str());
    CHECK(GameThreadImplementation::accepting.load(), "accepting");
    CHECK(g_cursor_ready, "cursor hook ready");
    CHECK(GameThreadImplementation::entity_context_hooks_ready.load(), "entity context hooks ready");
    for (const Place& p : kPlaces) CHECK(live(p)[0] == 0xE9, "%s carries the installer's jump", p.name);
    run_all_and_check("no host, everything hooked");
    EngineHooks::Uninstall();
    for (const Place& p : kPlaces) CHECK(bytes_are_file(p), "%s restored", p.name);
    CHECK(!GameThreadImplementation::accepting.load(), "not accepting after uninstall");

    // ---- 2. no host, another module hooked entity_manager first: SF cannot find the place any more and gives up ----
    std::puts("== no host, a foreign jump on entity_manager");
    foreign_hook(kPlaces[1]);
    CHECK(!EngineHooks::Install(), "install must refuse");
    // The live scan no longer finds the signature: the jump replaced its first bytes.
    CHECK(GameThreadImplementation::initialization_error == "entity-manager:signature-not-unique-or-missing", "error '%s'",
          GameThreadImplementation::initialization_error.c_str());
    CHECK(bytes_are_file(kPlaces[0]), "game_thread rolled back");
    CHECK(live(kPlaces[1])[0] == 0xE9, "the foreign jump is untouched");
    CHECK(!GameThreadImplementation::accepting.load(), "not accepting");

    // ---- 3. a host: the foreign jump no longer matters ----
    std::puts("== host offered, the same foreign jump");
    KfcRuntimeHost host = make_host();
    CHECK(KfcRuntimeSetHost(&host), "host accepted");
    g_host.clear();
    CHECK(EngineHooks::Install(), "install via host failed: %s", GameThreadImplementation::initialization_error.c_str());
    CHECK(GameThreadImplementation::accepting.load(), "accepting");
    CHECK(g_cursor_ready && GameThreadImplementation::entity_context_hooks_ready.load(), "hook-ready flags");
    CHECK(g_host.names.size() == 6, "six hooks placed, got %zu", g_host.names.size());
    for (std::size_t i = 0; i < g_host.names.size() && i < 6; ++i) {
        CHECK(g_host.names[i] == kPlaces[i].name, "hook %zu is %s", i, g_host.names[i].c_str());
        CHECK(g_host.targets[i] == reinterpret_cast<std::uintptr_t>(kPlaces[i].fn), "%s target", kPlaces[i].name);
        const std::uint32_t kind = kPlaces[i].cursor ? KFC_HOST_HOOK_CURSOR : kPlaces[i].context ? KFC_HOST_HOOK_CONTEXT : KFC_HOST_HOOK_PLAIN;
        CHECK(g_host.kinds[i] == kind, "%s kind %u", kPlaces[i].name, g_host.kinds[i]);
        CHECK(g_host.extras[i] == (kPlaces[i].cursor ? 0x270u : 0u), "%s extra %llx", kPlaces[i].name,
              (unsigned long long)g_host.extras[i]);
    }
    CHECK(g_host.callbacks.size() == 6 && g_host.callbacks[0] == reinterpret_cast<void*>(&GameThreadImplementation::drain) &&
              g_host.callbacks[1] == reinterpret_cast<void*>(&GameThreadImplementation::capture_entity_manager) &&
              g_host.callbacks[2] == reinterpret_cast<void*>(&WorldRuntime::OnPropUpdate) &&
              g_host.callbacks[3] == reinterpret_cast<void*>(&WorldRuntime::OnActorPlacement) &&
              g_host.callbacks[4] == reinterpret_cast<void*>(&WorldRuntime::OnBuildingDispatch) &&
              g_host.callbacks[5] == reinterpret_cast<void*>(&WorldRuntime::OnCursorUpdate),
          "callbacks");
    // The installer wrote nothing: the foreign jump and every other place are as they were.
    CHECK(live(kPlaces[1])[0] == 0xE9 && live(kPlaces[1])[5] == 0x90, "foreign jump untouched");
    for (int i : {0, 2, 3, 4, 5}) CHECK(bytes_are_file(kPlaces[i]), "%s not written by the installer", kPlaces[i].name);
    // What a host does with a hook: call the callback with the registers of its kind.
    {
        reset_seen();
        for (std::size_t i = 0; i < 6; ++i) {
            if (g_host.kinds[i] == KFC_HOST_HOOK_PLAIN)
                reinterpret_cast<void (*)(void*, void*)>(g_host.callbacks[i])(reinterpret_cast<void*>(0x15), reinterpret_cast<void*>(0xbb));
            else if (g_host.kinds[i] == KFC_HOST_HOOK_CONTEXT)
                reinterpret_cast<void (*)(void*, void*)>(g_host.callbacks[i])(reinterpret_cast<void*>(0xc7), nullptr);
            else
                reinterpret_cast<void (*)(void*, void*, void*)>(g_host.callbacks[i])(reinterpret_cast<void*>(0x14 + g_host.extras[i]),
                                                                                  reinterpret_cast<void*>(0x13), reinterpret_cast<void*>(0xbb));
        }
        for (std::size_t i = 0; i < 6; ++i) CHECK(g_seen[i].calls == 1, "%s callback reachable", kPlaces[i].name);
        CHECK(g_seen[0].a == 0x15 && g_seen[0].b == 0xbb && g_seen[4].a == 0xc7 && g_seen[5].a == 0x14 + 0x270,
              "callback arguments");
    }
    EngineHooks::Uninstall();
    CHECK(g_host.removed.size() == 6, "six hooks removed, got %zu", g_host.removed.size());
    CHECK(g_host.removed == std::vector<std::uint64_t>({105, 104, 103, 102, 101, 100}), "removed newest first");
    CHECK(live(kPlaces[1])[0] == 0xE9, "Uninstall left the foreign jump alone");
    CHECK(!GameThreadImplementation::accepting.load(), "not accepting after uninstall");

    // ---- 4. a host that cannot do part of it: the same fail-soft results as without a host ----
    std::puts("== host failures");
    g_host.clear();
    g_host.ambiguous_name = "entity_manager";
    CHECK(!EngineHooks::Install(), "ambiguous entity_manager refuses");
    CHECK(GameThreadImplementation::initialization_error == "entity-manager:signature-not-unique-or-missing", "error '%s'",
          GameThreadImplementation::initialization_error.c_str());
    CHECK(g_host.removed == std::vector<std::uint64_t>({100}), "game_thread's hook was taken back");
    g_host.clear();
    g_host.unavailable = "yes";
    CHECK(!EngineHooks::Install(), "unavailable host refuses");
    CHECK(GameThreadImplementation::initialization_error == "game-thread:host-signature-search-unavailable", "error '%s'",
          GameThreadImplementation::initialization_error.c_str());
    g_host.clear();
    g_host.refuse_name = "game_thread";
    CHECK(!EngineHooks::Install(), "host refusing game_thread");
    CHECK(GameThreadImplementation::initialization_error == "game-thread:host-hook-refused", "error '%s'",
          GameThreadImplementation::initialization_error.c_str());
    // The optional hooks (prop, placement, dispatch, cursor) failing only clears their ready flags.
    g_host.clear();
    g_host.refuse_name = "world_cursor";
    CHECK(EngineHooks::Install(), "cursor refused: install still succeeds: %s", GameThreadImplementation::initialization_error.c_str());
    CHECK(!g_cursor_ready && GameThreadImplementation::entity_context_hooks_ready.load(), "only the cursor flag is off");
    EngineHooks::Uninstall();
    // A profile whose original bytes differ from the file's: the host says MISMATCH.
    g_host.clear();
    namespace P = KfcRuntimeCompatibility::EnshroudedClient;
    P::game_thread_original[0] ^= 0xff;
    CHECK(!EngineHooks::Install(), "profile original differs from the file");
    CHECK(GameThreadImplementation::initialization_error == "game-thread:original-bytes-mismatch", "error '%s'",
          GameThreadImplementation::initialization_error.c_str());
    P::game_thread_original[0] ^= 0xff;

    // ---- 5. the table is checked; a bad one changes nothing; NULL takes the host away ----
    std::puts("== SetHost");
    KfcRuntimeHost small = make_host();
    small.size = static_cast<uint32_t>(offsetof(KfcRuntimeHost, remove_hook));
    CHECK(!KfcRuntimeSetHost(&small), "short table refused");
    KfcRuntimeHost no_find = make_host();
    no_find.find_signature = nullptr;
    CHECK(!KfcRuntimeSetHost(&no_find), "null function refused");
    KfcRuntimeHost old_version = make_host();
    old_version.version = 0;
    CHECK(!KfcRuntimeSetHost(&old_version), "version 0 refused");
    g_host.clear();
    CHECK(EngineHooks::Install() && g_host.names.size() == 6, "the earlier host is still the host");
    EngineHooks::Uninstall();
    CHECK(KfcRuntimeSetHost(nullptr), "NULL clears the host");
    DWORD old;  // put the file's bytes back on entity_manager: no foreign jump any more
    VirtualProtect(live(kPlaces[1]), 16, PAGE_EXECUTE_READWRITE, &old);
    std::memcpy(live(kPlaces[1]), pristine(kPlaces[1]), 16);
    FlushInstructionCache(GetCurrentProcess(), live(kPlaces[1]), 16);
    g_host.clear();
    CHECK(EngineHooks::Install(), "no host again: the installer hooks the game itself");
    CHECK(g_host.names.empty(), "the old host was not asked");
    CHECK(live(kPlaces[0])[0] == 0xE9, "own trampoline in place");
    run_all_and_check("no host again");
    EngineHooks::Uninstall();
    for (const Place& p : kPlaces) CHECK(bytes_are_file(p), "%s restored at the end", p.name);

    // ---- 6. the host's voxel writer (Kiln dev.31): optional, both members inside the offered size ----
    std::puts("== voxel writer");
    const std::int32_t at[3]{-3, 64, 7};
    const std::uint32_t box[3]{2, 1, 2};
    const std::uint16_t cells[4]{0x0000, 0xff81, 0x8001, 0x0000};
    std::uint32_t outcome = 9;
    CHECK(!EngineHooks::HostVoxelWriter(), "no host: no host writer");
    CHECK(!EngineHooks::HostVoxelWrite(at, box, cells, 4, &outcome) && outcome == 1, "no host: refused, outcome %u", outcome);
    KfcRuntimeHost v1 = make_host();  // a host that offers hooks only (members NULL)
    CHECK(KfcRuntimeSetHost(&v1) && !EngineHooks::HostVoxelWriter(), "a hooks-only host has no writer");
    KfcRuntimeHost cut = make_host();
    cut.voxel_write_ready = &fake_voxel_ready;
    cut.voxel_write = &fake_voxel_write;
    cut.size = static_cast<uint32_t>(offsetof(KfcRuntimeHost, voxel_write));  // an older host's table: writer outside it
    CHECK(KfcRuntimeSetHost(&cut) && !EngineHooks::HostVoxelWriter(), "members past the offered size are not read");
    KfcRuntimeHost half = make_host();
    half.voxel_write = &fake_voxel_write;
    CHECK(KfcRuntimeSetHost(&half) && !EngineHooks::HostVoxelWriter(), "one member alone is no writer");
    KfcRuntimeHost full = make_host();
    full.voxel_write_ready = &fake_voxel_ready;
    full.voxel_write = &fake_voxel_write;
    CHECK(KfcRuntimeSetHost(&full) && EngineHooks::HostVoxelWriter(), "a host with both members writes");
    g_voxels.ready = 0;
    CHECK(!EngineHooks::HostVoxelWriteReady(), "ready follows the host (no world)");
    g_voxels.ready = 1;
    CHECK(EngineHooks::HostVoxelWriteReady(), "ready follows the host (a hosted world)");
    outcome = 9;
    CHECK(EngineHooks::HostVoxelWrite(at, box, cells, 4, &outcome) && outcome == 0, "written: outcome %u", outcome);
    CHECK(g_voxels.calls == 1 && !std::memcmp(g_voxels.origin, at, sizeof at) && !std::memcmp(g_voxels.dims, box, sizeof box) &&
          g_voxels.values == std::vector<std::uint16_t>(cells, cells + 4), "the host got the box and the cells unchanged");
    for (const auto& [result, reported, ok, want] : {std::tuple{false, 1u, false, 1u}, {false, 2u, false, 2u}, {false, 3u, false, 3u},
             {false, 0u, false, 3u}, {false, 7u, false, 3u}, {true, 1u, false, 3u}}) {
        g_voxels.result = result;
        g_voxels.outcome = reported;
        outcome = 9;
        const bool got = EngineHooks::HostVoxelWrite(at, box, cells, 4, &outcome);
        CHECK(got == ok && outcome == want, "host %d/%u -> %d/%u (want %d/%u)", result, reported, got, outcome, ok, want);
    }
    CHECK(KfcRuntimeSetHost(nullptr) && !EngineHooks::HostVoxelWriter(), "NULL takes the writer away too");

    std::printf("%s (%d failure(s))\n", g_failures ? "FAILED" : "ALL PASSED", g_failures);
    return g_failures ? 1 : 0;
}
