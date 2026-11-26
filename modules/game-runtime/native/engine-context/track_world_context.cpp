#include "player-input/world_cursor_state.h"
#include "player-input/apply_building_input.h"
#include "engine-context/observe_entity_manager.h"
#include "engine-context/world_context_state.h"
#include "engine-context/engine_owners.h"
#include "voxels/read_registered_voxel_grid.h"
#include "impact/submit_entity_destruction.h"
#include "impact/submit_building_prop_spawn.h"
#include "voxels/write_registered_voxel_grid.h"
#include "engine-hooks/install_engine_hooks.h"

using namespace WorldRuntime::Implementation;

namespace WorldRuntime::Implementation {
std::atomic<std::uintptr_t> observed_actor_world{};

std::atomic<std::uint64_t> observed_actor_world_ms{};

// Pin the actual editable grid world after a successful read/write, matching
// Shroudtopia. The actor-placement hook also sees transient preview worlds.
std::atomic<std::uintptr_t> preferred_voxel_world{};

std::atomic<std::uint64_t> prop_update_hook_hits{}, cursor_hook_hits{}, context_resets{};

std::atomic<std::uintptr_t> local_execution_root{};

std::atomic<std::uintptr_t> client_read_world{}, client_read_manager{};

std::atomic<std::uint64_t> client_read_ms{}, client_read_generation{};

std::atomic<ActorProbe> actor_probe{ActorProbe::Never};

bool read_memory(std::uintptr_t address, void* output, std::size_t size){
    SIZE_T read{};
    return address && output && size && ReadProcessMemory(GetCurrentProcess(),
        reinterpret_cast<const void*>(address), output, size, &read) && read == size;
}

const KfcRuntimeCompatibility::EnshroudedClient::RuntimeOperation* find_operation(const char* name){
    if (!name) return nullptr;
    const auto& operations = KfcRuntimeCompatibility::EnshroudedClient::runtime_operations;
    const auto found = std::find_if(operations.begin(), operations.end(), [name](const auto& op) {
        return op.name == name;
    });
    return found == operations.end() ? nullptr : &*found;
}

bool valid_world_context(std::uintptr_t world){
    const auto* profile = find_operation("runtime.world.context.active");
    std::uintptr_t store{};
    return world && profile && profile->available && profile->validation_offset &&
        read_memory(world + profile->validation_offset, &store, sizeof(store)) && store;
}

bool resolve_voxel_world(std::uintptr_t& world){
    const auto* profile = find_operation("runtime.world.context.active");
    const auto base = reinterpret_cast<std::uintptr_t>(GetModuleHandleW(nullptr));
    std::uintptr_t singleton{}, context{};
    if (!profile || !profile->available) return false;
    // Dedicated servers have no client singleton. Their context must come
    // from a recently observed engine execution frame on this world.
    if (profile->actor_context) {
        if (GetTickCount64() - observed_actor_world_ms.load(std::memory_order_acquire) > 500) return false;
        const auto current = observed_actor_world.load(std::memory_order_acquire);
        if (!valid_world_context(current)) return false;
        world = current;
        return true;
    }

    const auto preferred = preferred_voxel_world.load(std::memory_order_acquire);
    if (valid_world_context(preferred)) { world = preferred; return true; }
    if (preferred) preferred_voxel_world.store(0, std::memory_order_release);

    // Prefer the validated actor-world context captured from ShroudForge's
    // build-verified actor hooks. The singleton is the fallback when the
    // current actor frame has not published a usable world yet.
    const auto actor_candidate = observed_actor_world.load(std::memory_order_acquire);
    if (valid_world_context(actor_candidate)) { world = actor_candidate; return true; }

    if (profile->global_rva && profile->context_pointer_offset && profile->world_offset &&
        read_memory(base + profile->global_rva, &singleton, sizeof(singleton)) && singleton) {
        const auto context_slot = static_cast<std::uintptr_t>(static_cast<std::intptr_t>(singleton) + profile->context_pointer_offset);
        if (read_memory(context_slot, &context, sizeof(context)) && context) {
            const auto signed_world = static_cast<std::intptr_t>(context) + profile->world_offset;
            if (signed_world > 0) {
                const auto candidate = static_cast<std::uintptr_t>(signed_world);
                if (valid_world_context(candidate)) { world = candidate; return true; }
            }
        }
    }
    return false;
}

bool resolve_client_read_world(std::uintptr_t& world){
    const auto generation = context_resets.load(std::memory_order_acquire);
    const auto observed = client_read_ms.load(std::memory_order_acquire);
    const auto candidate = client_read_world.load(std::memory_order_acquire);
    if (!observed || GetTickCount64() - observed > 500 ||
        client_read_generation.load(std::memory_order_acquire) != generation ||
        client_read_manager.load(std::memory_order_acquire) != GameThreadDispatcher::EntityManager() ||
        !valid_world_context(candidate) || context_resets.load(std::memory_order_acquire) != generation) return false;
    world = candidate;
    return true;
}

void observe_client_read_world(void* cursor_frame, void* execution_view){
    const auto& layout = KfcRuntimeCompatibility::EnshroudedClient::world_context_layout;
    if (!cursor_frame || !layout.client_cursor_service_view || !layout.client_cursor_service_world) return;
    std::uintptr_t service{}, world{}, root{}, frame_manager{};
    std::uint64_t count{};
    const auto manager = GameThreadDispatcher::EntityManager();
    if (!manager || !read_memory(reinterpret_cast<std::uintptr_t>(execution_view), &root, sizeof(root)) || !root ||
        !read_memory(root + KfcRuntimeCompatibility::EnshroudedClient::lookup_manager, &frame_manager, sizeof(frame_manager)) || frame_manager != manager ||
        !read_memory(manager + KfcRuntimeCompatibility::EnshroudedClient::entity_manager_count, &count, sizeof(count)) || !count || count > (1u<<20) ||
        !read_memory(reinterpret_cast<std::uintptr_t>(cursor_frame) + layout.client_cursor_service_view, &service, sizeof(service)) ||
        !read_memory(service + layout.client_cursor_service_world, &world, sizeof(world)) || !valid_world_context(world)) return;
    client_read_world.store(world, std::memory_order_release);
    client_read_manager.store(manager, std::memory_order_release);
    client_read_generation.store(context_resets.load(std::memory_order_acquire), std::memory_order_release);
    client_read_ms.store(GetTickCount64(), std::memory_order_release);
}

void observe_actor_world(void* actor_frame){
    if (!actor_frame) { actor_probe.store(ActorProbe::MissingFrame); return; }
    const auto& layout = KfcRuntimeCompatibility::EnshroudedClient::world_context_layout;
    if (!layout.actor_frame_service_view || !layout.service_view_world) {
        actor_probe.store(ActorProbe::MissingLayout); return;
    }
    const auto frame = reinterpret_cast<std::uintptr_t>(actor_frame);
    std::uintptr_t service_view{}, candidate{};
    if (!read_memory(frame + layout.actor_frame_service_view, &service_view, sizeof(service_view)) || !service_view) {
        actor_probe.store(ActorProbe::MissingService); return;
    }
    if (!read_memory(service_view + layout.service_view_world, &candidate, sizeof(candidate)) ||
        !valid_world_context(candidate)) { actor_probe.store(ActorProbe::InvalidWorld); return; }
    actor_probe.store(ActorProbe::Valid);
    observed_actor_world.store(candidate, std::memory_order_release);
    observed_actor_world_ms.store(GetTickCount64(), std::memory_order_release);
}

bool actor_frame_matches_active_world(void* actor_frame){
    if (!actor_frame) return false;
    const auto& layout = KfcRuntimeCompatibility::EnshroudedClient::world_context_layout;
    std::uintptr_t service_view{}, actor_world{}, active_world{};
    const auto frame = reinterpret_cast<std::uintptr_t>(actor_frame);
    if (!layout.actor_frame_service_view || !layout.service_view_world ||
        !read_memory(frame + layout.actor_frame_service_view, &service_view, sizeof(service_view)) || !service_view ||
        !read_memory(service_view + layout.service_view_world, &actor_world, sizeof(actor_world)) || !actor_world ||
        !resolve_voxel_world(active_world)) return false;
    return actor_world == active_world;
}

std::uintptr_t image_base(){ return reinterpret_cast<std::uintptr_t>(GetModuleHandleW(nullptr)); }
}

namespace WorldRuntime {
namespace {
std::uint32_t ecs_context_kind() {
    // Status/Diagnostics also call this while holding ECS state_mutex. Resolve
    // ownership directly, without recursively querying the session registry.
    EngineContext::EcsView view, again;
    if (!EngineContext::ResolveEcs(view) || !EngineContext::ResolveEcs(again) || again != view) return 0;
    // Ownership classifies the target. Each mutation still needs its own
    // operation contract and execution gate before it may change that target.
    return view.role == EngineContext::EcsRole::LocalAuthority ? 1u : 2u;
}
}
void ResetContext() {
    BuildingInput::Reset();
    observed_actor_world_ms.store(0, std::memory_order_release);
    observed_actor_world.store(0, std::memory_order_release);
    preferred_voxel_world.store(0, std::memory_order_release);
    observed_execution_view.store(0, std::memory_order_release);
    local_execution_root.store(0, std::memory_order_release);
    actor_probe.store(ActorProbe::Never);
    context_resets.fetch_add(1, std::memory_order_relaxed);
    client_read_ms.store(0, std::memory_order_release);
    client_read_world.store(0, std::memory_order_release);
    client_read_manager.store(0, std::memory_order_release);
}

bool OperationAvailable(const char* name) {
    if (name && (std::strcmp(name, "runtime.world.voxel.read") == 0 ||
                 std::strcmp(name, "runtime.world.voxel.grid_spec") == 0))
        return RegisteredVoxelGrid::Available();
    if (name && std::strcmp(name, "runtime.world.context.active") == 0)
        return ActiveContextAvailable();
    if (name && std::strcmp(name, "runtime.world.building.input") == 0)
        return BuildingInput::Available();
    if (name && std::strcmp(name, "runtime.world.cursor.get") == 0)
        return InterlockedCompareExchange(&cursor_mailbox.hook_ready, 0, 0) != 0 || LocalCursorInputAvailable();
    if (name && std::strcmp(name, "runtime.world.entity.destroy") == 0 &&
        ImpactRuntime::EntityDestructionAvailable()) return true;
    if (name && std::strcmp(name, "runtime.world.entity.spawn") == 0)
        return ImpactRuntime::BuildingPropSpawnAvailable();
    if (name && std::strcmp(name, "runtime.world.voxel.write") == 0)  // Kiln fork: the host's writer first
        return EngineHooks::HostVoxelWriter() ? EngineHooks::HostVoxelWriteReady() : RegisteredVoxelGrid::WriteAvailable();
    const auto* operation = find_operation(name);
    return operation && operation->available;
}

bool EntityContextReady() {
    std::uintptr_t world{};
    return GameThreadDispatcher::EntityContextReady() && resolve_voxel_world(world) &&
        OperationAvailable("runtime.world.entity.spawn") &&
        OperationAvailable("runtime.world.entity.place") &&
        OperationAvailable("runtime.world.entity.destroy") &&
        OperationAvailable("runtime.world.entity.finish_building");
}

bool ActiveContextAvailable() {
    if (ecs_context_kind()) return true;
    std::uintptr_t world{};
    return GameThreadDispatcher::Ready() && (resolve_voxel_world(world) || resolve_client_read_world(world));
}

std::uint32_t ContextKind() {
    // This is a classification query, not a write/readiness assertion. The
    // Lua caller also requires a live ECS session; keeping this independent of
    // the dispatcher's short readiness window lets the client distinguish a
    // validated remote cursor-world from an unknown context during a brief
    // game-thread stall. Actual world operations retain their own readiness
    // and permission checks.
    if (const auto kind = ecs_context_kind()) return kind;
    std::uintptr_t world{};
    if (resolve_voxel_world(world)) return 1;
    if (resolve_client_read_world(world)) return 2;
    return 0;
}

std::string ContextDiagnostics() {
    const auto* profile = find_operation("runtime.world.context.active");
    std::uintptr_t world{}, singleton{}, context{}, candidate{};
    const bool direct = resolve_voxel_world(world);
    const bool client_read = !direct && resolve_client_read_world(world);
    const auto ecs_kind = ecs_context_kind();
    const bool resolved = ecs_kind || direct || client_read;
    const char* global_reason = "profile-unavailable";
    if (profile && profile->available) {
        global_reason = "not-configured";
        if (profile->global_rva && profile->context_pointer_offset && profile->world_offset) {
            const auto base = reinterpret_cast<std::uintptr_t>(GetModuleHandleW(nullptr));
            global_reason = "singleton-unreadable";
            if (read_memory(base + profile->global_rva, &singleton, sizeof(singleton))) {
                global_reason = "singleton-null";
                if (singleton) {
                    global_reason = "context-unreadable";
                    if (read_memory(singleton + profile->context_pointer_offset, &context, sizeof(context))) {
                        global_reason = "context-null";
                        if (context) {
                            const auto address = static_cast<std::intptr_t>(context) + profile->world_offset;
                            candidate = address > 0 ? static_cast<std::uintptr_t>(address) : 0;
                            global_reason = valid_world_context(candidate) ? "valid" : "voxel-store-invalid";
                        }
                    }
                }
            }
        }
    }
    const auto actor = observed_actor_world.load(std::memory_order_acquire);
    const auto preferred = preferred_voxel_world.load(std::memory_order_acquire);
    const auto observed = observed_actor_world_ms.load(std::memory_order_acquire);
    const char* actor_reason = "not-observed";
    switch (actor_probe.load()) {
    case ActorProbe::MissingFrame: actor_reason = "frame-null"; break;
    case ActorProbe::MissingLayout: actor_reason = "layout-unavailable"; break;
    case ActorProbe::MissingService: actor_reason = "service-view-unavailable"; break;
    case ActorProbe::InvalidWorld: actor_reason = "voxel-store-invalid"; break;
    case ActorProbe::Valid: actor_reason = "validated-at-observation"; break;
    default: break;
    }
    const char* source = ecs_kind == 1 ? "ecs-local-authority" : ecs_kind == 2 ? "ecs-client-replica" :
        !resolved ? "none" : client_read ? "client-cursor-read" : world == preferred ? "pinned-world" :
        world == actor ? "actor-frame" : "client-singleton";
    return nlohmann::json({{"revision", "world-context-session-20261009-a2"}, {"resolved", resolved}, {"source", source},
        {"directWorldAvailable", direct}, {"clientReadWorld", client_read_world.load()},
        {"processRole", KfcRuntimeCompatibility::EnshroudedClient::image_target == "enshrouded_server.exe" ? "dedicated-server" : "client"},
        // Process role and context source cannot identify a client's network mode.
        {"clientSessionMode", "unknown"}, {"resets", context_resets.load()},
        {"global", {{"reason", global_reason}, {"singleton", singleton}, {"context", context}, {"candidate", candidate}}},
        {"actor", {{"reason", actor_reason}, {"candidate", actor},
            {"ageMs", observed ? nlohmann::json(GetTickCount64() - observed) : nlohmann::json(nullptr)}}},
        {"hooks", {{"propUpdate", prop_update_hook_hits.load()}, {"actorPlacement", actor_placement_hook_hits.load()},
            {"cursor", cursor_hook_hits.load()}, {"localExecutionRoot", local_execution_root.load()}}}
    }).dump();
}

std::string ContextStatus() {
    const auto evidence = nlohmann::json::parse(ContextDiagnostics());
    return "revision=world-context-session-20261009-a2,source=" + evidence["source"].get<std::string>() +
        ",global=" + evidence["global"]["reason"].get<std::string>() +
        ",actor=" + evidence["actor"]["reason"].get<std::string>() +
        ",prop-updates=" + std::to_string(prop_update_hook_hits.load()) +
        ",cursor=" + std::to_string(cursor_hook_hits.load());
}
}

extern "C" {
bool __cdecl KfcRuntimeWorldOperationAvailable(const char* name) { return WorldRuntime::OperationAvailable(name); }

bool __cdecl KfcRuntimeWorldContextActive() { return WorldRuntime::ActiveContextAvailable(); }

std::uint32_t __cdecl KfcRuntimeWorldContextKind() { return WorldRuntime::ContextKind(); }

bool __cdecl KfcRuntimeWorldEntityContextReady() { return WorldRuntime::EntityContextReady(); }
}
