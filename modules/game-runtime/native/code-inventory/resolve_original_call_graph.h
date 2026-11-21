#pragma once
#include <Zydis/Zydis.h>
#include "original_file_policy.h"
#include <optional>
#include <array>
#include <cstdint>
#include <functional>
#include <vector>

namespace OriginalCode {
// Read-only instruction evidence. Resolving code does not grant an invocation
// contract. Each typed adapter validates its own arguments, effects and owners.
struct Instruction {
    std::uintptr_t address{};
    ZydisDecodedInstruction decoded{};
    std::array<ZydisDecodedOperand, ZYDIS_MAX_OPERAND_COUNT> operands{};
    std::array<std::uint8_t,15> bytes{};
};
struct Procedure {
    struct ReadOnlyData { std::uintptr_t address{}; std::vector<std::uint8_t> bytes; };
    std::uintptr_t entry{};
    std::optional<OriginalFile::Detour> entry_detour;
    std::vector<Instruction> instructions;
    std::vector<std::uintptr_t> calls;
    bool indirect_calls{};
    std::vector<ReadOnlyData> read_only_data;
};

// Loaded-image section metadata. Immutable readable sections carry copied
// bytes. Writable sections expose bounds only, never a retained data snapshot.
struct ImageSection {
    std::uintptr_t address{};
    std::size_t size{};
    std::uint32_t flags{};
    std::vector<std::uint8_t> bytes;
};
bool ReadImageSections(std::vector<ImageSection>& output);

// Exact decoded bytes, coalesced into bounded contiguous reads. Both native
// adapters use this same evidence owner. Validation allocates no memory and
// compares all retained bytes, including decoded switch tables.
struct EvidenceBlock { std::uintptr_t address{}; std::vector<std::uint8_t> bytes; };
using CodeEvidence = std::vector<EvidenceBlock>;
bool AppendEvidence(const Procedure& procedure, CodeEvidence& output);
bool EvidenceUnchanged(const CodeEvidence& evidence);
// Exact entry bytes, span, target and indirect cell are rechecked against loaded memory.
bool DetoursUnchanged(const std::vector<OriginalFile::Detour>& evidence);
// ShroudForge's own code patches (the native main menu's call sites). The executable-file check accepts exactly the
// recorded replacement over exactly the recorded original bytes and still compares every other byte; a function
// holding one is never read through the file. These low-level records require externally serialized writes;
// production patchers use ApplyOwnPatch / RestoreOwnPatch below.
// At most OriginalFile::own_patch_limit patches of OriginalFile::own_patch_bytes bytes, inside the game image.
bool RecordOwnPatch(std::uintptr_t address, const std::uint8_t* original, const std::uint8_t* replacement, std::size_t size);
void ForgetOwnPatch(std::uintptr_t address);
// Production patchers serialize the record and the actual write/restore with original-code readers.
bool ApplyOwnPatch(std::uintptr_t address, const std::uint8_t* original, const std::uint8_t* replacement,
    std::size_t size, const std::function<bool()>& write);
bool RestoreOwnPatch(std::uintptr_t address, const std::function<bool()>& write);

// Complete chained PE unwind ranges, or a bounded call-free leaf CFG. Never
// treats E8 bytes inside operands as calls and never scans for byte signatures.
bool ReadProcedure(std::uintptr_t entry, Procedure& output);
// Like ReadProcedure, but a procedure another module detoured is read from the
// executable file and reported as foreign. A foreign procedure shows the
// original call graph only. It is not evidence and must not be retained as such.
// entry_detour describes the loaded entry, not file code; retain it separately
// and use DetoursUnchanged wherever invocation evidence is revalidated.
bool ReadOriginalProcedure(std::uintptr_t entry, Procedure& output, bool& foreign);
// Resolves an interior instruction through the canonical chained unwind owner.
// The address must be an instruction boundary reached by that owner's CFG.
bool ReadContainingProcedure(std::uintptr_t address, Procedure& output);
bool ResolveNamedSystemCode(const char* exact_name, std::uintptr_t& output);
// Kiln layer: one line into ShroudForge's log through the same sink as the file check's notes ('I', 'W' or 'E';
// the message ends with a newline).
void Diagnostic(char level, const char* message);
// Kiln layer: why ResolveNamedSystemCode last failed on the calling thread (a constant text or a thread-local buffer).
const char* LastResolveFailure();
bool FindUniqueReachable(std::uintptr_t root, unsigned maximum_depth,
    const std::function<bool(const Procedure&)>& predicate, std::uintptr_t& output);
}
