#include "voxels/voxel_request.h"
#include "engine-context/world_context_state.h"
#include "voxels/read_registered_voxel_grid.h"
#include "voxels/write_registered_voxel_grid.h"
#include "named-services/named_service_contract.h"
#include "engine-hooks/install_engine_hooks.h"

using namespace WorldRuntime::Implementation;

namespace WorldRuntime::Implementation {
bool plan_write_region(const std::int32_t* origin, const std::uint32_t* dimensions, WriteRegion& output) {
    std::size_t requested{};
    if (!origin || !cell_count(dimensions,requested)) return false;
    WriteRegion plan;
    plan.count=1;
    for (int axis=0; axis<3; ++axis) {
        const auto low=static_cast<std::int64_t>(origin[axis]);
        const auto high=low+dimensions[axis];
        auto chunk_low=low/8, chunk_high=high/8;
        if (low<0 && low%8) --chunk_low;
        if (high>0 && high%8) ++chunk_high;
        const auto aligned_low=chunk_low*8, aligned_high=chunk_high*8;
        if (aligned_low<INT32_MIN || aligned_low>INT32_MAX || aligned_high>INT32_MAX ||
            aligned_high<=aligned_low || static_cast<std::uint64_t>(aligned_high-aligned_low)>MaximumCells/plan.count)
            return false;
        plan.origin[axis]=static_cast<std::int32_t>(aligned_low);
        plan.dimensions[axis]=static_cast<std::uint32_t>(aligned_high-aligned_low);
        plan.offset[axis]=static_cast<std::size_t>(low-aligned_low);
        plan.count*=plan.dimensions[axis];
    }
    output=plan;
    return true;
}

bool safe_read(NativeRead function, CellSpan* span, const std::uint32_t* dimensions,
               const void* world, std::uint32_t mode, const std::int32_t* origin){
    __try { return function(span, dimensions, world, mode, origin); }
    __except(EXCEPTION_EXECUTE_HANDLER) { return false; }
}

void execute(void* opaque){
    auto& op = *static_cast<Operation*>(opaque);
    // The legacy game-thread reader never accepts a mutation request. Public
    // writes use the named, session-pinned Store adapter exclusively.
    if (op.kind != Operation::Kind::Read) return;
    std::uintptr_t world{};
    const bool direct_context = resolve_voxel_world(world);
    if (!direct_context && (op.kind != Operation::Kind::Read || !resolve_client_read_world(world))) return;
    if (op.kind == Operation::Kind::Read && op.context_generation != context_resets.load(std::memory_order_acquire)) return;
    const auto* read_profile = find_operation("runtime.world.voxel.read");
    const auto base = reinterpret_cast<std::uintptr_t>(GetModuleHandleW(nullptr));
    if (!read_profile || !read_profile->available) return;
    auto read_fn = reinterpret_cast<NativeRead>(base + read_profile->function_rva);
    CellSpan span{op.cells.data(), op.cells.size()};
    if (op.kind == Operation::Kind::Read) {
        op.result = safe_read(read_fn, &span, op.dimensions.data(), reinterpret_cast<void*>(world), read_profile->mode, op.origin.data());
        if (op.result && direct_context) {
            std::uintptr_t unset{};
            preferred_voxel_world.compare_exchange_strong(unset, world, std::memory_order_acq_rel);
        }
        return;
    }
}

bool cell_count(const std::uint32_t dimensions[3], std::size_t& count){
    if (!dimensions) return false;
    count = 1;
    for (int axis = 0; axis < 3; ++axis) {
        if (!dimensions[axis] || dimensions[axis] > MaximumCells / count) return false;
        count *= dimensions[axis];
    }
    return count <= MaximumCells;
}
}

namespace WorldRuntime {
bool ReadVoxels(const std::int32_t origin[3], const std::uint32_t dimensions[3],
                std::uint16_t* values, std::size_t capacity, std::size_t* actual) {
    std::size_t count{};
    if (!origin || !values || !actual || !cell_count(dimensions, count) || count > capacity ||
        !OperationAvailable("runtime.world.voxel.read") || !GameThreadDispatcher::Ready()) return false;
    auto op = std::make_shared<Operation>();
    op->kind = Operation::Kind::Read;
    op->context_generation = context_resets.load(std::memory_order_acquire);
    std::copy_n(origin, 3, op->origin.begin());
    std::copy_n(dimensions, 3, op->dimensions.begin());
    op->cells.resize(count);
    if (!GameThreadDispatcher::Invoke(execute, op, 500) || !op->result) return false;
    for (std::size_t i = 0; i < count; ++i)
        values[i] = static_cast<std::uint16_t>(op->cells[i].material | (static_cast<std::uint16_t>(op->cells[i].density) << 8));
    *actual = count;
    return true;
}

bool WriteVoxels(const std::int32_t origin[3], const std::uint32_t dimensions[3],
                 const std::uint16_t* values, std::size_t count, std::uint32_t* outcome) {
    if (!outcome) return false;
    *outcome = 1;
    std::size_t expected{};
    WriteRegion expanded;
    if (!origin || !values || !cell_count(dimensions, expected) || count != expected ||
        !plan_write_region(origin, dimensions, expanded)) return false;
    // Kiln fork: a host with its own voxel writer writes instead (abi/runtime_host.h, voxel_write).
    if (EngineHooks::HostVoxelWriter()) return EngineHooks::HostVoxelWrite(origin, dimensions, values, count, outcome);
    const auto session = KfcRuntimeWorldSessionId();
    if (!session) return false;
    constexpr ULONGLONG retry_budget_ms = 250;
    const auto started = GetTickCount64();
    for (;;) {
        if (KfcRuntimeWorldSessionId() != session) return false;
        const bool result = RegisteredVoxelGrid::WriteForSession(session, origin,
            dimensions, values, count, outcome);
        // Only outcome 1 guarantees that no native mutation was dispatched.
        // Every attempt has released its operation, session and Scene guards
        // before returning here. Never retry a verified rollback or uncertainty.
        if (result || *outcome != 1) return result;
        const auto elapsed = GetTickCount64() - started;
        if (elapsed >= retry_budget_ms) return false;
        Sleep(static_cast<DWORD>((std::min)(ULONGLONG{2}, retry_budget_ms - elapsed)));
        if (GetTickCount64() - started >= retry_budget_ms) return false;
    }
}
}

extern "C" {
bool __cdecl KfcRuntimeWorldVoxelRead(const std::int32_t* origin, const std::uint32_t* dimensions,
    std::uint16_t* values, std::size_t capacity, std::size_t* actual) {
    return WorldRuntime::ReadVoxels(origin, dimensions, values, capacity, actual);
}

std::uint32_t __cdecl KfcRuntimeWorldVoxelReadForSession(std::uint64_t expected_session,
    const std::int32_t* origin, const std::uint32_t* dimensions,
    std::uint16_t* values, std::size_t capacity, std::size_t* actual) {
    using WorldRuntime::RegisteredVoxelGrid::Result;
    const auto registered = WorldRuntime::RegisteredVoxelGrid::ReadForSession(
        expected_session, origin, dimensions, values, capacity, actual);
    return static_cast<std::uint32_t>(registered);
}

bool __cdecl KfcRuntimeWorldVoxelWrite(const std::int32_t* origin, const std::uint32_t* dimensions,
    const std::uint16_t* values, std::size_t count, std::uint32_t* outcome) {
    return WorldRuntime::WriteVoxels(origin, dimensions, values, count, outcome);
}
}
