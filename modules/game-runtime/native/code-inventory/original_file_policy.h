#pragma once
// Windows-independent admission policy; readers and instruction decoder are supplied by the host.
#include <algorithm>
#include <array>
#include <cstdint>
#include <cstring>
#include <iterator>
#include <map>
#include <set>
#include <span>
#include <vector>
namespace OriginalFile {
constexpr std::size_t function_limit = 4096;
constexpr unsigned depth_limit = 8;
constexpr std::size_t procedure_limit = 1024 * 1024;
constexpr std::size_t executable_limit = 128 * 1024 * 1024;
constexpr std::size_t image_limit = 512 * 1024 * 1024;
struct Section {
    std::array<std::uint8_t,8> name{};
    std::uint32_t rva{}, size{}, raw{}, raw_size{}, flags{};
    bool operator==(const Section&) const = default;
};
struct Layout {
    std::uint64_t preferred{}, loaded{};
    std::uint32_t size{}, timestamp{}, checksum{};
    std::uint16_t machine{}, count{};
    std::vector<Section> sections;
};
struct Range { std::uint32_t begin{}, end{}; };
struct Detour {
    std::uint32_t rva{}, span{};
    std::uint64_t target{}, cell{}, cell_value{};
    std::array<std::uint8_t,64> bytes{};
    bool operator==(const Detour&) const = default;
};
enum class Admission { live, detour, refuse };
inline const char* identity(const Layout& live, const Layout& file) {
    if (live.loaded != live.preferred) return "relocated image";
    if (!live.size || live.size > image_limit || file.size > image_limit) return "image size bound";
    if (file.preferred != live.preferred || file.size != live.size) return "image base/size";
    if (file.machine != live.machine) return "machine";
    if (file.timestamp != live.timestamp) return "timestamp";
    if (file.checksum != live.checksum) return "checksum";
    if (file.count != live.count || file.sections.size()!=live.count || live.sections.size()!=live.count) return "section count";
    std::size_t total{};
    for (std::size_t i=0;i<live.sections.size();++i) {
        const auto& a=live.sections[i]; const auto& b=file.sections[i];
        if (a!=b) return "section layout";
        if (a.rva>live.size || a.size>live.size-a.rva) return "section range";
        if (a.flags & 0x20000000u) {
            if (a.flags & 0x80000000u) return "writable executable section";
            if (a.size>executable_limit-total) return "executable section budget";
            total+=a.size;
        }
    }
    return nullptr;
}
inline bool bounded(std::span<const Range> ranges, std::uint32_t image_size) {
    std::size_t total{};
    std::uint32_t previous{};
    if (ranges.empty()) return false;
    for (const auto r:ranges) {
        if (r.begin<previous || r.begin>=r.end || r.end>image_size || r.end-r.begin>procedure_limit-total) return false;
        total+=r.end-r.begin; previous=r.end;
    }
    return true;
}
inline bool add(std::uint64_t base, std::int64_t displacement, std::uint64_t& result) {
    if (displacement>=0) {
        if (static_cast<std::uint64_t>(displacement)>UINT64_MAX-base) return false;
        result=base+static_cast<std::uint64_t>(displacement);
    } else {
        const auto distance=std::uint64_t{0}-static_cast<std::uint64_t>(displacement);
        if (distance>base) return false;
        result=base-distance;
    }
    return true;
}
// Where a detour may lead: committed executable memory of any image or allocation, never a guard page. Hook
// trampolines and stubs are private allocations and another loader's code is its own image, so the game image's own
// code is not the test. Values as VirtualQuery reports them (State, Protect).
inline bool executable_page(std::uint32_t state, std::uint32_t protect) {
    constexpr std::uint32_t committed=0x1000, guard=0x100;
    if (state!=committed || (protect & guard)) return false;
    switch (protect & 0xff) {
    case 0x10: case 0x20: case 0x40: case 0x80: return true;  // EXECUTE, _READ, _READWRITE, _WRITECOPY
    default: return false;
    }
}
// No procedure-sized allocation: compare fixed chunks, then inspect at most 64 entry bytes.
// Decode returns the length of the FILE instruction at a supplied byte buffer.
template<class Read, class Decode, class Executable>
Admission admit(std::uint64_t base, std::uint32_t entry, std::span<const Range> ranges,
    Read file, Read live, Decode decode, Executable executable, Detour& out) {
    out={};
    if (!bounded(ranges,UINT32_MAX) || base>UINT64_MAX-entry) return Admission::refuse;
    const auto first=std::find_if(ranges.begin(),ranges.end(),[&](auto r){return r.begin==entry;});
    if (first==ranges.end()) return Admission::refuse;
    std::array<std::uint8_t,4096> a{}, b{};
    bool different=false;
    std::size_t changed_end{};
    for (const auto r:ranges) for (auto at=r.begin;at<r.end;) {
        const auto n=(std::min)(a.size(),static_cast<std::size_t>(r.end-at));
        if (base>UINT64_MAX-at || n>UINT64_MAX-(base+at) ||
            !file(base+at,a.data(),n)||!live(base+at,b.data(),n)) return Admission::refuse;
        for (std::size_t i=0;i<n;++i) if (a[i]!=b[i]) {
            different=true;
            if (at+i<entry || at+i-entry>=64) return Admission::refuse;
            changed_end=(std::max)(changed_end,static_cast<std::size_t>(at+i-entry+1));
        }
        at+=static_cast<std::uint32_t>(n);
    }
    if (!different) return Admission::live;
    const auto available=(std::min)(std::size_t{64},static_cast<std::size_t>(first->end-entry));
    std::array<std::uint8_t,64> original{}, patched{};
    const auto address=base+entry;
    if (!file(address,original.data(),available)||!live(address,patched.data(),available)) return Admission::refuse;
    std::size_t encoding{};
    std::uint64_t destination{}, cell{}, cell_value{};
    std::int32_t disp{};
    if (available>=5 && patched[0]==0xe9) {
        encoding=5; std::memcpy(&disp,patched.data()+1,4);
        if (!add(address+5,disp,destination)) return Admission::refuse;
    } else if (available>=6 && patched[0]==0xff && patched[1]==0x25) {
        encoding=6;std::memcpy(&disp,patched.data()+2,4);
        if (!add(address+6,disp,cell)||!live(cell,&destination,8)) return Admission::refuse;
        cell_value=destination;
        // The absolute literal may occupy the overwritten entry (Kiln's far form).
        if (cell==address+6) {
            if (available<14) return Admission::refuse;
            encoding=14;
        } else if ((cell>=address && cell-address<6) || (cell<address && address-cell<8)) return Admission::refuse;
    } else if (available>=12 && patched[0]==0x48 && patched[1]==0xb8 && patched[10]==0xff && patched[11]==0xe0) {
        encoding=12;std::memcpy(&destination,patched.data()+2,8);
    } else if (available>=13 && patched[0]==0x49 && patched[1]==0xbb && patched[10]==0x41 && patched[11]==0xff && patched[12]==0xe3) {
        encoding=13;std::memcpy(&destination,patched.data()+2,8);
    } else return Admission::refuse;
    for (const auto r:ranges) if (destination>=base+r.begin && destination<base+r.end) return Admission::refuse;
    if (!destination || !executable(destination)) return Admission::refuse;
    std::size_t span{};
    while (span<(std::max)(encoding,changed_end)) {
        if (span>=available) return Admission::refuse;
        const auto length=decode(original.data()+span,available-span);
        if (!length || length>available-span) return Admission::refuse;
        span+=length;
    }
    // Only NOP/INT3 fill after the transfer (including extra reserved whole instructions).
    for (auto i=encoding;i<span;) {
        if (patched[i]==0x90 || patched[i]==0xcc) {++i;continue;}
        auto opcode=i;
        while (opcode<span && (patched[opcode]==0x66 || patched[opcode]==0x2e)) ++opcode;
        const bool nop=opcode<span && (patched[opcode]==0x90 ||
            (span-opcode>=3 && patched[opcode]==0x0f && patched[opcode+1]==0x1f && !(patched[opcode+2]&0x38)));
        const auto length=nop?decode(patched.data()+i,span-i):0;
        if (!length || length>span-i) return Admission::refuse;
        i+=length;
    }
    out.rva=entry;out.span=static_cast<std::uint32_t>(span);out.target=destination;out.cell=cell;out.cell_value=cell_value;
    std::copy_n(patched.begin(),span,out.bytes.begin());
    return Admission::detour;
}
// Whole-image identity only. A procedure containing an interior jump is never read through the file: admit() above
// still refuses it, so it is never a traversal link and never selected.
// The jump is one transfer written over whole ORIGINAL instructions, as a detour library's mid hook writes it
// (safetyhook, Kiln's hook registry): `E9 rel32`, `FF 25 00000000` with the 8-byte absolute target inline, or
// `FF 25 disp32` through an 8-byte cell outside the span; then 0x90 up to the end of the fewest whole original
// instructions that hold the transfer, and no further. Its target lies outside this image and is executable.
constexpr std::size_t interior_span_limit = 32;
constexpr std::size_t interior_function_limit = 8;
constexpr std::size_t interior_image_limit = 256;
struct Interior { std::uint32_t rva{}, span{}; std::uint64_t target{}, cell{}; };
// `original` (file) and `patched` (loaded) hold `window` bytes at `site`, an original instruction boundary.
template<class Read, class Decode, class Executable>
bool transfer_at(std::uint64_t base, std::uint32_t image_size, std::uint32_t site, std::uint32_t changed,
    const std::uint8_t* original, const std::uint8_t* patched, std::size_t window,
    Read live, Decode decode, Executable executable, Interior& out) {
    const auto address=base+site;
    std::size_t encoding{};
    std::uint64_t destination{}, cell{};
    std::int32_t disp{};
    if (window>=5 && patched[0]==0xe9) {
        encoding=5;std::memcpy(&disp,patched+1,4);
        if (!add(address+5,disp,destination)) return false;
    } else if (window>=6 && patched[0]==0xff && patched[1]==0x25) {
        std::memcpy(&disp,patched+2,4);
        if (!disp) {
            if (window<14) return false;
            encoding=14;std::memcpy(&destination,patched+6,8);
        } else {
            encoding=6;
            if (!add(address+6,disp,cell)) return false;
        }
    } else return false;
    std::size_t span{};
    while (span<encoding) {
        const auto length=decode(original+span,window-span);
        if (!length || length>window-span) return false;
        span+=length;
    }
    if (span>interior_span_limit || changed<site || changed-site>=span) return false;
    for (auto i=encoding;i<span;++i) if (patched[i]!=0x90) return false;
    if (cell && ((cell>=address && cell-address<span) || (cell<address && address-cell<8))) return false;
    if (cell && !live(cell,&destination,8)) return false;
    if (!destination || (destination>=base && destination-base<image_size) || !executable(destination)) return false;
    out={site,static_cast<std::uint32_t>(span),destination,cell};
    return true;
}
// `changed` is the first differing RVA in the unwind range `part`; every byte from `floor` to it equals the file.
// Walks the range's original instructions from its start (an unwind range begins at an instruction) and tries each
// boundary in [floor, changed] less than interior_span_limit bytes before the change.
template<class Read, class Decode, class Executable>
bool interior(std::uint64_t base, std::uint32_t image_size, Range part, std::uint32_t floor, std::uint32_t changed,
    Read file, Read live, Decode decode, Executable executable, Interior& out) {
    out={};
    if (part.begin>=part.end || part.end>image_size || changed<part.begin || changed>=part.end || floor>changed ||
        part.end-part.begin>procedure_limit || base>UINT64_MAX-image_size) return false;
    std::array<std::uint8_t,64> original{}, patched{};
    for (auto site=part.begin;site<=changed;) {
        const auto window=(std::min)(original.size(),static_cast<std::size_t>(part.end-site));
        if (!file(base+site,original.data(),window)) return false;
        const auto step=decode(original.data(),window);
        if (!step || step>window) return false;
        if (site>=floor && changed-site<interior_span_limit) {
            if (!live(base+site,patched.data(),window)) return false;
            if (transfer_at(base,image_size,site,changed,original.data(),patched.data(),window,live,decode,executable,out))
                return true;
        }
        site+=static_cast<std::uint32_t>(step);
    }
    out={};
    return false;
}
struct CodeSection { std::uint32_t rva{}; std::span<const std::uint8_t> bytes; };
struct ImageCheck { const char* reason{}; std::uint32_t rva{}; std::size_t entries{}, interiors{}; };
// The loaded executable sections against the file's, once at first use: every byte equal, or inside an entry detour
// admitted by `classify` (admit() of the owning procedure), or inside an interior jump (above).
// `loaded` reads the loaded image by RVA; `file` and `live` read by address, as for admit().
template<class Loaded, class Classify, class Read, class Decode, class Executable>
ImageCheck compare_image(std::uint64_t base, std::uint32_t image_size, std::span<const CodeSection> sections,
    const std::map<std::uint32_t,std::uint32_t>& owners, const std::map<std::uint32_t,std::vector<Range>>& procedures,
    Loaded loaded, Classify classify, Read file, Read live, Decode decode, Executable executable) {
    std::array<std::uint8_t,4096> current{};
    std::set<std::uint32_t> entries;
    std::map<std::uint32_t,std::size_t> per_function;
    std::size_t interiors{};
    for (const auto& section:sections) {
        std::size_t offset{};
        auto floor=section.rva;
        while (offset<section.bytes.size()) {
            const auto n=(std::min)(current.size(),section.bytes.size()-offset);
            const auto rva=section.rva+static_cast<std::uint32_t>(offset);
            if (!loaded(rva,current.data(),n)) return {"unreadable loaded code",rva};
            const auto* original=section.bytes.data()+offset;
            if (!std::memcmp(original,current.data(),n)) {offset+=n;continue;}
            std::size_t changed{};
            while (changed<n && original[changed]==current[changed]) ++changed;
            const auto at=rva+static_cast<std::uint32_t>(changed);
            const auto owner=owners.upper_bound(at);
            if (owner==owners.begin()) return {"code outside every function differs from the file",at};
            const auto root=std::prev(owner)->second;
            Detour detour;
            if (classify(root,detour)==Admission::detour && at>=detour.rva && at-detour.rva<detour.span) {
                entries.insert(detour.rva);
                if (entries.size()>function_limit) return {"entry detour count bound",root};
                offset=detour.rva+detour.span-section.rva;
                floor=detour.rva+detour.span;
                continue;
            }
            const auto found=procedures.find(root);
            const Range* part{};
            if (found!=procedures.end() && bounded(found->second,image_size))
                for (const auto& r:found->second) if (r.begin<=at && at<r.end) part=&r;
            Interior jump;
            if (!part || !interior(base,image_size,*part,floor,at,file,live,decode,executable,jump))
                return {"function differs from the file other than by an entry detour or an interior jump",root};
            if (++per_function[root]>interior_function_limit) return {"interior jump count bound in one function",root};
            if (++interiors>interior_image_limit) return {"interior jump count bound",root};
            offset=jump.rva+jump.span-section.rva;
            floor=jump.rva+jump.span;
        }
    }
    return {nullptr,0,entries.size(),interiors};
}
inline bool selectable(Admission a) { return a == Admission::live; }
}
