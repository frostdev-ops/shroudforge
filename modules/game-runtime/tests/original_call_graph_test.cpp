// Production decoder against this test PE and controlled read-only image views.
// --inspect maps an original EXE without resolving imports or running its code.
#include <windows.h>
namespace {
HMODULE inspected_image{};
HMODULE graph_test_image(LPCWSTR) {
    return inspected_image ? inspected_image : GetModuleHandleW(nullptr);
}
}
#define GetModuleHandleW graph_test_image
#include "code-inventory/resolve_original_call_graph.cpp"
#undef GetModuleHandleW
#include <cstdio>
#include <cstdlib>
#include <source_location>

namespace {
unsigned checks{};
std::uintptr_t failed_copy_at{}, unstable_copy_at{}, denied_code_at{};
unsigned unstable_reads{};
std::vector<std::uint8_t>* image_view{};

void check(bool value, std::source_location where = std::source_location::current()) {
    ++checks;
    if (!value) {
        std::fprintf(stderr, "original call graph check %u failed at line %u\n", checks, where.line());
        std::exit(1);
    }
}
std::uintptr_t address(const void* value) { return reinterpret_cast<std::uintptr_t>(value); }
template<class F> std::uintptr_t code_address(F function) { return reinterpret_cast<std::uintptr_t>(function); }

__declspec(noinline) unsigned leaf_a(unsigned value) { return value ^ 0xe8e8e8e8u; }
__declspec(noinline) unsigned leaf_b(unsigned value) { return value ^ 0x11223344u; }
__declspec(noinline) unsigned branch_a(unsigned value) {
    volatile auto result = leaf_a(value);
    return result + 1;
}
__declspec(noinline) unsigned branch_b(unsigned value) {
    volatile auto result = leaf_b(value);
    return result + 2;
}
__declspec(noinline) unsigned directed_root(unsigned value) {
    volatile auto first = branch_a(value);
    volatile auto second = branch_b(value);
    return first + second;
}
// Patched in place before the executable file is first read; never called.
__declspec(noinline) unsigned detour_fixture(unsigned value) {
    volatile auto first = leaf_a(value);
    volatile auto second = leaf_b(first);
    return first * 3u + second;
}
__declspec(noinline) unsigned mid_hook_fixture(unsigned value) {
    volatile auto first = leaf_b(value);
    volatile auto second = leaf_a(first ^ 0x5au);
    volatile auto third = leaf_b(second + 7u);
    return first + second * 5u + third;
}
__declspec(noinline) unsigned call_site_fixture(unsigned value) {
    volatile auto first = leaf_a(value + 3u);
    return first ^ 0x77u;
}
unsigned (*volatile const patch_fixtures[])(unsigned) = {detour_fixture, mid_hook_fixture, call_site_fixture};

struct NativeDependency { const char* name; std::uint64_t size; };
struct NativeDescriptor {
    const char* name;
    std::uint64_t name_size;
    unsigned (*callback)(unsigned);
    const NativeDependency* dependencies;
    std::uint64_t dependency_count;
    std::uint64_t other_slots[10];
};
static_assert(sizeof(NativeDescriptor) == 120);
const char dependency_name[] = "keen::ecs::CurrentTransform";
const NativeDependency dependencies[] = {{dependency_name, sizeof(dependency_name) - 1}};
const char root_name[] = "graph_fixture_root", ambiguous_name[] = "graph_fixture_ambiguous";
const NativeDescriptor native_descriptors[] = {
    {root_name, sizeof(root_name)-1, directed_root, dependencies, 1, {}},
    {ambiguous_name, sizeof(ambiguous_name)-1, branch_a, dependencies, 1, {}},
    {ambiguous_name, sizeof(ambiguous_name)-1, branch_b, dependencies, 1, {}},
};

// Owned executable fixtures are decoded only, never invoked. The spare bytes
// keep each bounded leaf read inside this test image's executable section.
#pragma section(".graph", read, execute)
__declspec(allocate(".graph")) __declspec(align(16)) const std::uint8_t immediate_e8[1024] = {
    0xb8,0xe8,0xe8,0xe8,0xe8,0xc3,
};
__declspec(allocate(".graph")) __declspec(align(16)) const std::uint8_t conditional_leaf[1024] = {
    0x85,0xc9,0x74,0x06,0xb8,0x11,0,0,0,0xc3,0xb8,0x22,0,0,0,0xc3,
};
__declspec(allocate(".graph")) __declspec(align(16)) const std::uint8_t skipped_invalid[1024] = {
    0xeb,0x02,0xf0,0x90,0xc3,
};
__declspec(allocate(".graph")) __declspec(align(16)) const std::uint8_t overlapping_branch[1024] = {
    0x85,0xc9,0x74,0x01,0xb8,0,0,0,0,0xc3,
};
__declspec(allocate(".graph")) __declspec(align(16)) const std::uint8_t invalid_instruction[1024] = {
    0xf0,0x90,0xc3,
};
__declspec(allocate(".graph")) __declspec(align(16)) const std::uint8_t leaf_with_call[1024] = {
    0xe8,0,0,0,0,0xc3,
};
__declspec(allocate(".graph")) __declspec(align(16)) const std::uint8_t indirect_branch[1024] = {
    0xff,0xe0,
};
__declspec(allocate(".graph")) __declspec(align(16)) const std::uint8_t trap_leaf[1024] = {
    0x0f,0x0b,0xc3,
};
__declspec(allocate(".graph")) __declspec(align(16)) const std::uint8_t outside_leaf[1024] = {
    0xe9,0,0x02,0,0,
};

void expect_rejected(std::uintptr_t entry) {
    OriginalCode::Procedure output;
    output.entry = 0x12345678;
    check(!OriginalCode::ReadProcedure(entry, output));
    check(output.entry == 0x12345678 && output.instructions.empty() && output.calls.empty());
}

template<class T> void put(std::vector<std::uint8_t>& bytes, std::size_t offset, const T& value) {
    check(offset <= bytes.size() && sizeof(value) <= bytes.size() - offset);
    std::memcpy(bytes.data() + offset, &value, sizeof(value));
}
std::vector<std::uint8_t> unwind_image() {
    std::vector<std::uint8_t> bytes(0x4000);
    IMAGE_DOS_HEADER dos{};
    dos.e_magic = IMAGE_DOS_SIGNATURE;
    dos.e_lfanew = 0x80;
    put(bytes, 0, dos);
    IMAGE_NT_HEADERS64 nt{};
    nt.Signature = IMAGE_NT_SIGNATURE;
    nt.FileHeader.Machine = IMAGE_FILE_MACHINE_AMD64;
    nt.FileHeader.NumberOfSections = 1;
    nt.FileHeader.SizeOfOptionalHeader = sizeof(nt.OptionalHeader);
    nt.OptionalHeader.Magic = IMAGE_NT_OPTIONAL_HDR64_MAGIC;
    nt.OptionalHeader.SizeOfImage = static_cast<DWORD>(bytes.size());
    nt.OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_EXCEPTION] = {0x1000, 2*sizeof(RUNTIME_FUNCTION)};
    put(bytes, 0x80, nt);
    const RUNTIME_FUNCTION root{0x2000,0x2010,0x3000}, child{0x2100,0x2110,0x3020};
    put(bytes, 0x1000, root);
    put(bytes, 0x1000 + sizeof(root), child);
    bytes[0x3000] = 1;
    bytes[0x3020] = 1 | (UNW_FLAG_CHAININFO << 3);
    bytes[0x3022] = 3; // Odd code count must round the chained record to an even slot.
    put(bytes, 0x302c, root);
    return bytes;
}
bool image_rejected(std::vector<std::uint8_t>& bytes) {
    image_view = &bytes;
    bool rejected{};
    try { OriginalCode::Image parsed; rejected = !parsed.size; }
    catch (...) { rejected = true; }
    image_view = nullptr;
    return rejected;
}

void check_unwind_metadata() {
    auto bytes = unwind_image();
    image_view = &bytes;
    {
        OriginalCode::Image parsed;
        check(parsed.procedures.size() == 1 && parsed.ranges.size() == 2);
        const auto& ranges = parsed.procedures.at(0x2000);
        check(ranges.size() == 2 && ranges[0].begin == 0x2000 && ranges[1].begin == 0x2100);
        std::uint8_t value{};
        check(!parsed.Read(0x4000, &value, 1));
        check(!parsed.Read(UINT32_MAX, &value, 1));
        check(!parsed.Read(0x3fff, &value, 2));
    }
    image_view = nullptr;
    bytes[0x3000] = 2;
    check(!image_rejected(bytes));
    for (const auto version : {0,3,7}) {
        bytes = unwind_image(); bytes[0x3000] = static_cast<std::uint8_t>(version);
        check(image_rejected(bytes));
    }
    bytes = unwind_image(); bytes[0x3020] |= UNW_FLAG_EHANDLER << 3;
    check(image_rejected(bytes));
    bytes = unwind_image(); bytes[0x3000] = 1 | (8 << 3);
    check(image_rejected(bytes));
    bytes = unwind_image();
    put(bytes, 0x302c, RUNTIME_FUNCTION{0x2100,0x2110,0x3020});
    check(image_rejected(bytes));
    bytes = unwind_image(); put(bytes, 0x1000, RUNTIME_FUNCTION{0x2000,0x4001,0x3000});
    check(image_rejected(bytes));
    bytes = unwind_image(); put(bytes, 0x1000, RUNTIME_FUNCTION{0x2000,0x2010,0x3fff});
    check(image_rejected(bytes));
    bytes = unwind_image();
    auto* nt = reinterpret_cast<IMAGE_NT_HEADERS64*>(bytes.data() + 0x80);
    nt->OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_EXCEPTION].VirtualAddress = 0x3ff8;
    check(image_rejected(bytes));
    bytes = unwind_image();
    nt = reinterpret_cast<IMAGE_NT_HEADERS64*>(bytes.data() + 0x80);
    nt->OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_EXCEPTION].Size--;
    check(image_rejected(bytes));
}

void check_bounded_code_evidence() {
    std::array<std::uint8_t, 9000> bytes{};
    for (std::size_t i=0;i<bytes.size();++i) bytes[i]=static_cast<std::uint8_t>(i);
    OriginalCode::Procedure procedure;
    for (std::size_t offset=0;offset<4500;offset+=15) {
        OriginalCode::Instruction instruction;
        instruction.address=address(bytes.data()+offset);
        instruction.decoded.length=15;
        std::copy_n(bytes.data()+offset,15,instruction.bytes.begin());
        procedure.instructions.push_back(instruction);
    }
    procedure.read_only_data.push_back({address(bytes.data()+6000),{bytes.begin()+6000,bytes.end()}});
    OriginalCode::CodeEvidence evidence;
    check(OriginalCode::AppendEvidence(procedure,evidence));
    check(evidence.size()==3);
    check(OriginalCode::EvidenceUnchanged(evidence));
    bytes[4094]^=1;
    check(!OriginalCode::EvidenceUnchanged(evidence));
    bytes[4094]^=1;
    bytes[8800]^=1;
    check(!OriginalCode::EvidenceUnchanged(evidence));
    bytes[8800]^=1;
    failed_copy_at=evidence[1].address;
    check(!OriginalCode::EvidenceUnchanged(evidence));
    failed_copy_at=0;
    check(OriginalCode::EvidenceUnchanged(evidence));
    procedure.instructions[0].decoded.length=0;
    check(!OriginalCode::AppendEvidence(procedure,evidence) && evidence.size()==3);
    procedure.instructions[0].decoded.length=15;
    procedure.instructions[0].address=UINTPTR_MAX-1;
    check(!OriginalCode::AppendEvidence(procedure,evidence) && evidence.size()==3);
    check(!OriginalCode::EvidenceUnchanged({}));
    evidence[0].bytes.resize(4097);
    check(!OriginalCode::EvidenceUnchanged(evidence));
}

// A hook library's trampolines and stubs: private executable memory within rel32 reach of the image.
std::uint8_t* allocate_near(std::uintptr_t image, std::size_t image_size) {
    SYSTEM_INFO info{};
    GetSystemInfo(&info);
    const std::uintptr_t step = info.dwAllocationGranularity;
    const auto attempt = [](std::uintptr_t at) {
        return static_cast<std::uint8_t*>(VirtualAlloc(reinterpret_cast<void*>(at), 4096, MEM_RESERVE | MEM_COMMIT, PAGE_EXECUTE_READWRITE));
    };
    for (auto at = (image & ~(step - 1)) - step; at > step && image - at < 0x70000000; at -= step)
        if (auto* page = attempt(at)) return page;
    const auto end = (image + image_size + step - 1) & ~(step - 1);
    for (auto at = end; at - image < 0x70000000; at += step)
        if (auto* page = attempt(at)) return page;
    return nullptr;
}
void write_code(std::uintptr_t at, const std::uint8_t* bytes, std::size_t size) {
    DWORD previous{}, ignored{};
    check(VirtualProtect(reinterpret_cast<void*>(at), size, PAGE_EXECUTE_READWRITE, &previous) != FALSE);
    std::memcpy(reinterpret_cast<void*>(at), bytes, size);
    check(VirtualProtect(reinterpret_cast<void*>(at), size, previous, &ignored) != FALSE);
    FlushInstructionCache(GetCurrentProcess(), reinterpret_cast<void*>(at), size);
}
// The whole original instructions at `at` that hold `need` bytes, by the production decoder.
std::size_t whole_instructions(std::uintptr_t at, std::size_t need) {
    std::size_t span{};
    while (span < need) {
        const auto length = OriginalCode::instruction_length(reinterpret_cast<const std::uint8_t*>(at + span), 15);
        if (!length) return 0;
        span += length;
    }
    return span;
}
// Kiln's registry form: E9 rel32, then 0x90 over the rest of the overwritten instructions.
std::vector<std::uint8_t> kiln_jump(std::uintptr_t at, std::size_t span, std::uintptr_t target) {
    std::vector<std::uint8_t> bytes(span, 0x90);
    bytes[0] = 0xe9;
    const auto relative = static_cast<std::int32_t>(static_cast<std::intptr_t>(target) - static_cast<std::intptr_t>(at + 5));
    std::memcpy(bytes.data() + 1, &relative, 4);
    return bytes;
}
struct Patch { std::uintptr_t at{}; std::vector<std::uint8_t> bytes, original; };
Patch prepare_patch(std::uintptr_t at, std::vector<std::uint8_t> bytes) {
    Patch patch{at, std::move(bytes), {}};
    patch.original.assign(reinterpret_cast<const std::uint8_t*>(at), reinterpret_cast<const std::uint8_t*>(at) + patch.bytes.size());
    return patch;
}
bool calls(const OriginalCode::Procedure& code, std::uintptr_t callee) {
    return std::find(code.calls.begin(), code.calls.end(), callee) != code.calls.end();
}
Patch mid_hook;
// Before the executable file is first read (it is cached once per process): an entry detour into a private
// executable page, as Kiln's hook registry writes it over PlaceProp, and a mid hook in another function, as
// Kiln's Rules service writes them at every start. The image is still this file's image and the walk reads
// the detoured function through the file. Restored afterwards, so the later checks see unchanged code.
void check_patched_image() {
    const auto& im = OriginalCode::image();
    auto* stub = allocate_near(im.base, im.size);
    check(stub != nullptr);
    std::memset(stub, 0xc3, 64);
    const auto entry = code_address(detour_fixture), body = code_address(mid_hook_fixture);
    check(im.procedures.contains(static_cast<std::uint32_t>(entry - im.base)));
    const auto entry_span = whole_instructions(entry, 5);
    check(entry_span >= 5 && entry_span < 64);
    // The mid hook: the first original instruction boundary at least 16 bytes in, as the production decoder walks it.
    const auto found = im.procedures.find(static_cast<std::uint32_t>(body - im.base));
    check(found != im.procedures.end());
    auto site = body;
    while (site < body + 16) { const auto step = whole_instructions(site, 1); check(step != 0); site += step; }
    const auto site_span = whole_instructions(site, 5);
    check(site_span >= 5 && site + site_span <= im.base + found->second.front().end);
    mid_hook = prepare_patch(site, kiln_jump(site, site_span, address(stub) + 16));
    // ShroudForge's own main-menu kind of patch: one E8 call site redirected to a near thunk, recorded first.
    const auto caller = code_address(call_site_fixture);
    auto call = caller;
    while (*reinterpret_cast<const std::uint8_t*>(call) != 0xe8 || whole_instructions(call, 1) != 5) {
        const auto step = whole_instructions(call, 1);
        check(step != 0 && call - caller < 64);
        call += step;
    }
    auto redirected = kiln_jump(call, 5, address(stub) + 32);
    redirected[0] = 0xe8;
    const auto own = prepare_patch(call, redirected);
    check(OriginalCode::RecordOwnPatch(own.at, own.original.data(), own.bytes.data(), own.bytes.size()));
    std::vector<Patch> patches{prepare_patch(entry, kiln_jump(entry, entry_span, address(stub))), mid_hook, own};
    for (const auto& patch : patches) write_code(patch.at, patch.bytes.data(), patch.bytes.size());
    OriginalCode::Procedure code;
    bool foreign{};
    check(OriginalCode::ReadOriginalProcedure(entry, code, foreign));
    check(foreign && code.entry_detour && code.entry_detour->target == address(stub) && code.entry_detour->span == entry_span);
    check(OriginalCode::file_image().usable.load());
    check(calls(code, code_address(leaf_a)) && calls(code, code_address(leaf_b)));
    for (auto patch = patches.rbegin(); patch != patches.rend(); ++patch) write_code(patch->at, patch->original.data(), patch->original.size());
    OriginalCode::ForgetOwnPatch(own.at);
}

nlohmann::json failure_details(std::uintptr_t entry) {
    const auto& im = OriginalCode::image();
    nlohmann::json result{{"rva",entry >= im.base ? entry-im.base : 0}};
    if (entry < im.base || entry-im.base >= im.size) { result["reason"]="outside-image"; return result; }
    if (!NamedServiceImplementation::executable_callback(entry)) { result["reason"]="non-executable-image-page"; return result; }
    const auto found = im.procedures.find(static_cast<std::uint32_t>(entry-im.base));
    if (found == im.procedures.end()) {
        result["reason"] = "bounded-leaf-cfg-rejected";
        for (const auto& range : im.ranges)
            if (range.begin <= entry-im.base && entry-im.base < range.end) {
                result["reason"] = "internal-unwind-label";
                result["range_begin"] = range.begin;
                result["range_end"] = range.end;
            }
    } else {
        ZydisDecoder decoder;
        ZydisDecoderInit(&decoder, ZYDIS_MACHINE_MODE_LONG_64, ZYDIS_STACK_WIDTH_64);
        result["reason"] = "unwind-code-read-or-stability-failure";
        for (const auto& range : found->second) {
            const auto count = range.end-range.begin;
            std::vector<std::uint8_t> bytes(count);
            if (!OriginalCode::executable_span(im.base+range.begin, count)) {
                result["reason"]="unwind-range-not-executable"; result["range_begin"]=range.begin; break;
            }
            if (!im.Read(range.begin, bytes.data(), count)) break;
            for (std::size_t offset=0; offset<count;) {
                OriginalCode::Instruction ins;
                if (!OriginalCode::decode(decoder,im.base+range.begin+offset,bytes.data()+offset,count-offset,ins)) {
                    result["reason"]="unwind-instruction-decode-rejected";
                    result["instruction_rva"]=range.begin+offset;
                    result["remaining_bytes"]=count-offset;
                    return result;
                }
                offset += ins.decoded.length;
            }
        }
    }
    std::array<std::uint8_t,16> prefix{};
    if (im.Read(static_cast<std::uint32_t>(entry-im.base),prefix.data(),prefix.size())) result["prefix"]=prefix;
    return result;
}
nlohmann::json inspect_graph(std::uintptr_t root, const std::string& label, unsigned depth_limit) {
    const auto base = address(graph_test_image(nullptr));
    nlohmann::json result{{"root",label},{"root_rva",root-base},{"depth",depth_limit},
        {"entries",nlohmann::json::array()},{"failed",nlohmann::json::array()}};
    std::deque<std::pair<std::uintptr_t,unsigned>> pending{{root,0}};
    std::set<std::uintptr_t> visited;
    while (!pending.empty()) {
        const auto [entry,depth] = pending.front(); pending.pop_front();
        if (!visited.insert(entry).second) continue;
        if (visited.size()>4096) { result["truncated"]=true; break; }
        OriginalCode::Procedure procedure;
        if (!OriginalCode::ReadProcedure(entry,procedure)) { result["failed"].push_back(failure_details(entry)); continue; }
        nlohmann::json calls=nlohmann::json::array();
        for (const auto callee : procedure.calls) {
            calls.push_back(callee-base);
            if (depth<depth_limit) pending.emplace_back(callee,depth+1);
        }
        result["entries"].push_back({{"rva",entry-base},{"depth",depth},{"instructions",procedure.instructions.size()},
            {"calls",calls},{"indirect_calls",procedure.indirect_calls}});
    }
    result["visited"]=visited.size();
    result["failed_count"]=result["failed"].size();
    return result;
}
int inspect_original(int argc, wchar_t** argv) {
    inspected_image=LoadLibraryExW(argv[2],nullptr,DONT_RESOLVE_DLL_REFERENCES);
    if (!inspected_image) { std::fprintf(stderr,"image mapping failed: %lu\n",GetLastError()); return 2; }
    const auto base=address(inspected_image);
    const auto* dos=reinterpret_cast<const IMAGE_DOS_HEADER*>(base);
    const auto* nt=reinterpret_cast<const IMAGE_NT_HEADERS64*>(base+dos->e_lfanew);
    KfcRuntimeCompatibility::EnshroudedClient::image_size=nt->OptionalHeader.SizeOfImage;
    nlohmann::json report{{"mode","read-only-original-image-callgraph"},{"engine_calls",0},{"process_opened",false},
        {"image_size",nt->OptionalHeader.SizeOfImage},{"graphs",nlohmann::json::array()}};
    for (const auto* name : {"player_building_blueprints","player_building_place_prop","player_building_undo"}) {
        std::uintptr_t root{};
        if (!OriginalCode::ResolveNamedSystemCode(name,root)) report["graphs"].push_back({{"root",name},{"resolved",false}});
        else report["graphs"].push_back(inspect_graph(root,name,6));
    }
    for (int index=3; index<argc; ++index) {
        wchar_t* end{};
        const auto rva=std::wcstoull(argv[index],&end,0);
        if (!*argv[index] || *end || rva>=nt->OptionalHeader.SizeOfImage) return 2;
        report["graphs"].push_back(inspect_graph(base+static_cast<std::uintptr_t>(rva),"analysis-rva",6));
    }
    std::puts(report.dump(2).c_str());
    FreeLibrary(inspected_image);
    return 0;
}
}

namespace NamedServiceImplementation {
bool copy(void* to,const void* from,std::size_t size) {
    const auto at=address(from),base=address(graph_test_image(nullptr));
    if (at==failed_copy_at) return false;
    if (image_view && at>=base) {
        const auto offset=at-base;
        if (offset>image_view->size() || size>image_view->size()-offset) return false;
        std::memcpy(to,image_view->data()+offset,size);
        return true;
    }
    __try { std::memcpy(to,from,size); }
    __except(EXCEPTION_EXECUTE_HANDLER) { return false; }
    if (at==unstable_copy_at && size && ++unstable_reads==2) static_cast<std::uint8_t*>(to)[0]^=1;
    return true;
}
bool executable_callback(std::uintptr_t at) {
    if (!at || at==denied_code_at) return false;
    MEMORY_BASIC_INFORMATION page{};
    if (VirtualQuery(reinterpret_cast<const void*>(at),&page,sizeof(page))!=sizeof(page) ||
        page.State!=MEM_COMMIT || page.Type!=MEM_IMAGE || page.AllocationBase!=graph_test_image(nullptr)) return false;
    return page.Protect==PAGE_EXECUTE || page.Protect==PAGE_EXECUTE_READ;
}
}

int wmain(int argc,wchar_t** argv) {
    if (argc>=3 && !std::wcscmp(argv[1],L"--inspect")) return inspect_original(argc,argv);
    check(argc==1);
    const auto base=address(graph_test_image(nullptr));
    const auto* dos=reinterpret_cast<const IMAGE_DOS_HEADER*>(base);
    const auto* nt=reinterpret_cast<const IMAGE_NT_HEADERS64*>(base+dos->e_lfanew);
    KfcRuntimeCompatibility::EnshroudedClient::image_size=nt->OptionalHeader.SizeOfImage;
    const volatile auto* retain=native_descriptors;
    check(retain->callback==directed_root);
    check_unwind_metadata();
    check_bounded_code_evidence();

    OriginalCode::Procedure procedure;
    check(OriginalCode::ReadProcedure(address(immediate_e8),procedure));
    check(procedure.calls.empty() && !procedure.indirect_calls && procedure.instructions.size()==2);
    check(procedure.instructions[0].decoded.mnemonic==ZYDIS_MNEMONIC_MOV);
    check(OriginalCode::ReadProcedure(address(conditional_leaf),procedure));
    check(procedure.instructions.size()==6 && procedure.calls.empty());
    check(OriginalCode::ReadProcedure(address(skipped_invalid),procedure));
    check(procedure.instructions.size()==2 && procedure.calls.empty());
    check(OriginalCode::ReadProcedure(address(indirect_branch),procedure));
    check(procedure.indirect_calls && procedure.calls.empty());
    check(OriginalCode::ReadProcedure(address(outside_leaf),procedure));
    check(procedure.calls.size()==1 && procedure.calls[0]==address(outside_leaf)+517);
    check(OriginalCode::ReadProcedure(address(trap_leaf),procedure));
    check(procedure.instructions.size()==1 && procedure.instructions[0].decoded.mnemonic==ZYDIS_MNEMONIC_UD2);
    for (const auto* rejected : {overlapping_branch,invalid_instruction,leaf_with_call})
        expect_rejected(address(rejected));
    expect_rejected(0);
    expect_rejected(base-1);
    expect_rejected(base+nt->OptionalHeader.SizeOfImage);
    expect_rejected(address(native_descriptors));
    void* private_code=VirtualAlloc(nullptr,4096,MEM_COMMIT|MEM_RESERVE,PAGE_EXECUTE_READ);
    check(private_code!=nullptr); expect_rejected(address(private_code)); VirtualFree(private_code,0,MEM_RELEASE);
    check(!OriginalCode::executable_span(base+nt->OptionalHeader.SizeOfImage-1,2));
    check(!OriginalCode::executable_span(UINTPTR_MAX-1,4));
    check(!OriginalCode::executable_span(address(immediate_e8),0));
    denied_code_at=address(immediate_e8); expect_rejected(denied_code_at); denied_code_at=0;
    failed_copy_at=address(immediate_e8); expect_rejected(failed_copy_at); failed_copy_at=0;
    unstable_copy_at=address(immediate_e8); unstable_reads=0;
    expect_rejected(unstable_copy_at); check(unstable_reads==2); unstable_copy_at=0;

    check(OriginalCode::ReadProcedure(code_address(directed_root),procedure));
    check(std::find(procedure.calls.begin(),procedure.calls.end(),code_address(branch_a))!=procedure.calls.end());
    check(std::find(procedure.calls.begin(),procedure.calls.end(),code_address(branch_b))!=procedure.calls.end());
    expect_rejected(code_address(directed_root)+1);
    check(patch_fixtures[0]==detour_fixture);
    check_patched_image();
    std::uintptr_t selected=0x12345678;
    check(OriginalCode::ResolveNamedSystemCode(root_name,selected) && selected==code_address(directed_root));
    selected=0x12345678;
    check(!OriginalCode::ResolveNamedSystemCode(ambiguous_name,selected) && selected==0x12345678);
    check(!OriginalCode::ResolveNamedSystemCode("graph_fixture_missing",selected) && selected==0x12345678);
    check(!OriginalCode::ResolveNamedSystemCode(nullptr,selected));
    check(!OriginalCode::ResolveNamedSystemCode("",selected));
    const auto leaf_match=[](const OriginalCode::Procedure& code) { return code.entry==code_address(leaf_a); };
    check(OriginalCode::FindUniqueReachable(code_address(directed_root),2,leaf_match,selected) && selected==code_address(leaf_a));
    selected=0x12345678;
    check(!OriginalCode::FindUniqueReachable(code_address(directed_root),1,leaf_match,selected) && selected==0x12345678);
    check(!OriginalCode::FindUniqueReachable(code_address(directed_root),2,[](const auto& code) {
        return code.entry==code_address(branch_a) || code.entry==code_address(branch_b);
    },selected) && selected==0x12345678);
    failed_copy_at=code_address(leaf_b);
    check(!OriginalCode::FindUniqueReachable(code_address(directed_root),2,leaf_match,selected) && selected==0x12345678);
    failed_copy_at=0;
    check(!OriginalCode::FindUniqueReachable(code_address(directed_root),9,leaf_match,selected));
    check(!OriginalCode::FindUniqueReachable(0,2,leaf_match,selected));
    check(!OriginalCode::FindUniqueReachable(code_address(directed_root),2,{},selected));
    // Last: an indirect mid hook can still be decoded live as a terminal edge. Refusing it once must never
    // turn the next read into live-only admission, or a depth-zero walk can select the patched function.
    auto* retry_stub=allocate_near(base,nt->OptionalHeader.SizeOfImage);
    check(retry_stub!=nullptr);
    const auto retry_span=whole_instructions(mid_hook.at,14);
    const auto& retry_parts=OriginalCode::image().procedures.at(static_cast<std::uint32_t>(code_address(mid_hook_fixture)-base));
    check(retry_span>=14 && retry_span<=32 && mid_hook.at+retry_span<=base+retry_parts.front().end);
    std::vector<std::uint8_t> retry_bytes(retry_span,0x90);
    retry_bytes[0]=0xff; retry_bytes[1]=0x25;
    std::fill(retry_bytes.begin()+2,retry_bytes.begin()+6,0);
    const auto retry_target=address(retry_stub);
    std::memcpy(retry_bytes.data()+6,&retry_target,8);
    const auto retry_patch=prepare_patch(mid_hook.at,retry_bytes);
    write_code(retry_patch.at,retry_patch.bytes.data(),retry_patch.bytes.size());
    {
        OriginalCode::Procedure code;
        bool foreign{true};
        check(!OriginalCode::ReadOriginalProcedure(code_address(mid_hook_fixture), code, foreign) && !foreign);
        check(OriginalCode::ReadProcedure(code_address(mid_hook_fixture),code));
        check(!OriginalCode::ReadOriginalProcedure(code_address(mid_hook_fixture), code, foreign) && !foreign);
        selected=0x12345678;
        check(!OriginalCode::FindUniqueReachable(code_address(mid_hook_fixture),0,[](const auto&){return true;},selected));
        check(selected==0x12345678);
    }
    write_code(retry_patch.at,retry_patch.original.data(),retry_patch.original.size());
    VirtualFree(retry_stub,0,MEM_RELEASE);
    std::printf("original call graph checks passed: %u\n",checks);
}
