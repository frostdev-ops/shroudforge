#pragma once
#include <cstddef>
#include <cstdint>
struct KfcRuntimeHost;
namespace EngineHooks {
bool SetHost(const KfcRuntimeHost* host);  // Kiln fork: abi/runtime_host.h
// Kiln fork: the host's voxel writer (abi/runtime_host.h, voxel_write_ready / voxel_write). HostVoxelWriter: a host
// offered both. The other two answer false (and outcome 1) without one.
bool HostVoxelWriter();
bool HostVoxelWriteReady();
bool HostVoxelWrite(const std::int32_t* origin, const std::uint32_t* dimensions, const std::uint16_t* values,
    std::size_t count, std::uint32_t* outcome);
bool Install();
void Uninstall();
}
