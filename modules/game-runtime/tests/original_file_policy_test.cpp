#include "code-inventory/original_file_policy.h"
#include <cstdio>
#include <functional>
#include <map>
#include <string>
#include <Zydis/Zydis.h>
#include <atomic>
#include <chrono>
#include <future>
#include <thread>
using namespace OriginalFile;
int checks{}, failures{};
void check(bool ok, const char* name) { ++checks; if(!ok) { ++failures; std::printf("FAIL %s\n",name); } }
InstructionStep real_decode(const std::uint8_t* p,std::size_t n) {
    ZydisDecoder decoder;
    ZydisDecoderInit(&decoder,ZYDIS_MACHINE_MODE_LONG_64,ZYDIS_STACK_WIDTH_64);
    ZydisDecodedInstruction instruction{};
    std::array<ZydisDecodedOperand,ZYDIS_MAX_OPERAND_COUNT> operands{};
    if (!ZYAN_SUCCESS(ZydisDecoderDecodeFull(&decoder,p,n,&instruction,operands.data()))) return {0};
    InstructionStep step{instruction.length};
    const auto category=instruction.meta.category;
    step.fallthrough=category!=ZYDIS_CATEGORY_UNCOND_BR && category!=ZYDIS_CATEGORY_RET &&
        category!=ZYDIS_CATEGORY_INTERRUPT && instruction.mnemonic!=ZYDIS_MNEMONIC_UD2;
    if ((category==ZYDIS_CATEGORY_UNCOND_BR || category==ZYDIS_CATEGORY_COND_BR) &&
        operands[0].type==ZYDIS_OPERAND_TYPE_IMMEDIATE && operands[0].imm.is_relative) {
        step.branch=true; step.relative=operands[0].imm.value.s;
    }
    return step;
}
int main() {
    Layout a; a.preferred=a.loaded=0x100000; a.size=4096; a.machine=0x8664; a.count=1;
    a.timestamp=1; a.checksum=2; a.sections.push_back({{},0,256,512,256,0x60000020});
    check(!identity(a,a),"identical headers");
    auto b=a; b.sections[0].raw=1024; check(identity(a,b),"changed section layout");
    b=a; ++b.timestamp; check(identity(a,b),"changed timestamp");
    b=a; ++b.checksum; check(identity(a,b),"changed checksum");
    b=a; ++b.relocation_rva; check(identity(a,b),"changed relocation directory RVA");
    b=a; ++b.relocation_size; check(identity(a,b),"changed relocation directory size");
    b=a; ++b.machine; check(identity(a,b),"changed machine");
    b=a; ++b.count; check(identity(a,b),"changed section count");
    // A relocated (ASLR) image is this file's image when the loaded header names the loaded base (Windows and Wine
    // write it there) or still the file's; its code is then compared after the file's relocations (below).
    b=a; b.loaded+=0x10000; b.preferred=b.loaded; check(!identity(b,a),"relocated base, loaded header names the loaded base");
    b=a; b.loaded+=0x10000; check(!identity(b,a),"relocated base, loaded header still names the file's base");
    b=a; b.loaded+=0x10000; b.preferred+=0x20000; check(identity(b,a),"loaded header base neither the loaded nor the file's refused");
    b=a; b.size+=4096; check(identity(b,a),"image size differs");
    std::array<std::uint8_t,256> disk{}, memory{}; disk.fill(0x90); memory=disk;
    // Original prologue consists of 3-byte instructions, then single-byte NOPs.
    auto decode=[](const std::uint8_t* p,std::size_t n) -> std::size_t { return n && *p==0x55 ? (n>=3?3:0) : n>=4 && p[0]==0x0f && p[1]==0x1f ? 4 : n?1:0; };
    disk[0]=disk[3]=disk[6]=disk[9]=disk[12]=0x55; memory=disk;
    std::vector<Range> ranges{{0,128},{160,192}};
    using Reader=std::function<bool(std::uint64_t,void*,std::size_t)>;
    Reader file=[&](auto at,void* p,auto n){ if(at<a.loaded||at-a.loaded>disk.size()||n>disk.size()-(at-a.loaded))return false; std::memcpy(p,disk.data()+at-a.loaded,n); return true; };
    std::uint64_t cell_target=0x800000;
    Reader live=[&](auto at,void* p,auto n){ if(at==0x900000 && n==8){std::memcpy(p,&cell_target,8);return true;} if(at<a.loaded||at-a.loaded>memory.size()||n>memory.size()-(at-a.loaded))return false;std::memcpy(p,memory.data()+at-a.loaded,n);return true;};
    auto run=[&](Detour& d){return admit(a.loaded,0,ranges,file,live,decode,[](auto p){return p==0x800000;},d);};
    Detour d;
    check(run(d)==Admission::live,"identical code");
    disk[70]^=1; check(run(d)==Admission::refuse,"replaced file same size"); disk[70]^=1;
    memory[70]^=1; check(run(d)==Admission::refuse,"body patch"); memory=disk;
    auto rel=[&](std::uint64_t target){memory=disk;memory[0]=0xe9;auto disp=static_cast<std::int32_t>(target-a.loaded-5);std::memcpy(memory.data()+1,&disp,4);memory[5]=0x90;};
    rel(0x800000); check(run(d)==Admission::detour && d.span==6 && d.target==0x800000,"rel32 rounded span");
    memory[5]=0x0f;memory[6]=0x1f;memory[7]=0x40;memory[8]=0;
    check(run(d)==Admission::detour && d.span==9,"multi-byte NOP fill beyond minimum span");
    rel(0x800000);check(run(d)==Admission::detour,"reset rel32 record");
    const auto saved=d; memory[1]^=1; Detour changed; check(run(changed)==Admission::refuse || changed!=saved,"changed detour invalidates record");
    rel(0x800000); memory[70]^=1; check(run(d)==Admission::refuse,"entry plus body patch");
    rel(a.loaded+30); check(run(d)==Admission::refuse,"jump into same function");
    rel(0x800000); memory[170]^=1; check(run(d)==Admission::refuse,"chained range body patch");
    memory=disk; memory[0]=0x48;memory[1]=0xb8;std::uint64_t target=0x800000;std::memcpy(memory.data()+2,&target,8);memory[10]=0xff;memory[11]=0xe0;
    check(run(d)==Admission::detour && d.span==12 && d.target==target,"mov rax jmp rax");
    memory=disk; memory[0]=0x49;memory[1]=0xbb;std::memcpy(memory.data()+2,&target,8);memory[10]=0x41;memory[11]=0xff;memory[12]=0xe3;memory[13]=memory[14]=0xcc;
    check(run(d)==Admission::detour && d.span==15 && d.target==target,"mov r11 jmp r11 int3 padding");
    memory=disk;memory[0]=0xff;memory[1]=0x25;std::memset(memory.data()+2,0,4);std::memcpy(memory.data()+6,&target,8);memory[14]=0x90;
    check(run(d)==Admission::detour && d.span==15 && d.target==target,"Kiln far inline jump");
    memory=disk;memory[0]=0xff;memory[1]=0x25;auto disp=static_cast<std::int32_t>(0x900000-a.loaded-6);std::memcpy(memory.data()+2,&disp,4);
    check(run(d)==Admission::detour && d.span==6 && d.target==target && d.cell==0x900000,"rip external pointer");
    const auto cell_record=d;cell_target=0x800001;
    check(run(d)==Admission::refuse || d!=cell_record,"changed pointer cell invalidates record");cell_target=0x800000;
    memory=disk;memory[0]=0xff;memory[1]=0x25;auto inner=std::int32_t{14};std::memcpy(memory.data()+2,&inner,4);std::memcpy(memory.data()+20,&target,8);
    check(run(d)==Admission::refuse,"nonadjacent inline literal refused");
    memory=disk;target=0x800000;std::memcpy(disk.data()+80,&target,8);std::memcpy(memory.data()+80,&target,8);
    memory[0]=0xff;memory[1]=0x25;inner=74;std::memcpy(memory.data()+2,&inner,4);
    check(run(d)==Admission::detour && d.span==6 && d.target==target,"rip pointer in unchanged function data");
    disk.fill(0x90);disk[0]=disk[3]=disk[6]=disk[9]=disk[12]=0x55;memory=disk;
    check(!selectable(Admission::detour),"detoured helper never selected");
    check(resolution_admission(true,false,Admission::refuse)==Admission::refuse,
        "function policy: disabling file reads cannot select an interior-patched function on retry");
    check(resolution_admission(true,false,Admission::detour)==Admission::refuse,
        "function policy: disabling file reads cannot select an entry-detoured function on retry");
    check(resolution_admission(false,false,Admission::refuse)==Admission::live,
        "function policy: an image that never matched retains standalone live fallback");
    check(resolution_admission(true,false,Admission::live)==Admission::live,
        "function policy: unchanged standalone functions remain selectable after a refusal elsewhere");
    std::vector<Range> huge{{0,static_cast<std::uint32_t>(procedure_limit+1)}};
    check(!bounded(huge,static_cast<std::uint32_t>(procedure_limit+1)),"oversized range refused before read/allocation");
    unsigned reads{};
    Reader unread=[&](auto,void*,auto){++reads;return false;};
    check(admit(a.loaded,0,huge,unread,unread,decode,[](auto){return true;},d)==Admission::refuse && reads==0,"oversized range before reader");
    auto repeated=ranges; repeated.push_back({128,129});
    check(!bounded(repeated,256),"overlapping/out-of-order ranges");
    b=a; ++b.sections[0].name[0];check(identity(a,b),"section name");
    b=a; ++b.sections[0].rva;check(identity(a,b),"section RVA");
    b=a; ++b.sections[0].size;check(identity(a,b),"section virtual size");
    b=a; ++b.sections[0].raw_size;check(identity(a,b),"section raw size");
    memory=disk; memory[0]=0xff;memory[1]=0x25;std::memset(memory.data()+2,0,4);target=a.loaded+170;std::memcpy(memory.data()+6,&target,8);memory[14]=0x90;
    check(run(d)==Admission::refuse,"jump into chained range");
    target=0x800000;rel(target);auto first=d;check(run(first)==Admission::detour,"record capture");
    memory=disk;check(run(d)==Admission::live && d!=first,"removed detour invalidates record");
    rel(target);check(run(d)==Admission::detour && d==first,"unchanged detour record");
    memory[5]=0x42;check(run(d)==Admission::refuse,"non-padding overwritten suffix");
    memory=disk; memory[80]=0xe9;check(run(d)==Admission::refuse,"non-entry jump not admitted");
    rel(0x800000);
    Reader changing=[&](auto at,void* p,auto n) {
        const auto ok=live(at,p,n);
        if (n==64) memory[70]^=1; // Body changes after the full comparison, during the entry reread.
        return ok;
    };
    check(admit(a.loaded,0,ranges,file,changing,decode,[](auto){return true;},d)==Admission::refuse,
        "entry admission: body changed during the entry reread refused");
    memory=disk;
    memory=disk;rel(0x800000);
    std::vector<Range> short_range{{0,4}};
    check(admit(a.loaded,0,short_range,file,live,decode,[](auto){return true;},d)==Admission::refuse,"truncated entry");
    b=a;b.sections[0].size=executable_limit;b.size=image_limit;
    b.sections.push_back(b.sections[0]);b.sections[1].rva=executable_limit;b.sections[1].size=1;b.count=2;
    check(identity(b,b),"total executable budget");
    b=a;b.sections[0].flags|=0x80000000;check(identity(b,b),"writable executable section");
    b=a;b.size=image_limit+1;check(identity(b,b),"oversized image");
    b=a;b.sections[0].size=executable_limit+1;check(identity(b,b),"oversized executable section");
    // Detour targets: committed executable memory of any kind (VirtualQuery State and Protect values).
    check(executable_page(0x1000,0x20) && executable_page(0x1000,0x40) && executable_page(0x1000,0x10) &&
        executable_page(0x1000,0x80),"detour target: RX, RWX, X and WCX pages (trampolines are private RWX)");
    check(executable_page(0x1000,0x220),"detour target: executable page with a cache modifier");
    check(!executable_page(0x1000,0x04) && !executable_page(0x1000,0x02) && !executable_page(0x1000,0x01) &&
        !executable_page(0x1000,0x08),"detour target: data and no-access pages refused");
    check(!executable_page(0x1000,0x120),"detour target: guard page refused");
    check(!executable_page(0x2000,0x20) && !executable_page(0x10000,0x01) && !executable_page(0,0x20),"detour target: reserved or free memory refused");
    // Whole-image identity over byte buffers: one executable section, procedures with unwind ranges, and the
    // transfers a detour library's mid hook writes (safetyhook, Kiln's hook registry: E9 or FF 25, then 0x90).
    {
        // Hook memory within E9 reach of the image (rel32), outside it.
        constexpr std::uint64_t base=0x10000000, hook=base+0x40001000, cell_at=base+0x7f00;
        constexpr std::uint32_t size=0x8000, text=0x1000, text_size=0x5000;
        std::vector<std::uint8_t> disk(size,0xcc), memory;
        std::map<std::uint32_t,std::uint32_t> owners;
        std::map<std::uint32_t,std::vector<Range>> procedures;
        // Instruction lengths by first byte: 55 = 3, 48 = 7, E9 = 5, D6 = 40 (longer than any real one), 0F 1F = 4.
        auto x86=[](const std::uint8_t* p,std::size_t n) -> std::size_t {
            if (!n) return 0;
            std::size_t length=1;
            switch (p[0]) { case 0x55: length=3; break; case 0x48: length=7; break; case 0xe9: length=5; break;
                case 0xd6: length=40; break; case 0x0f: length=n>=2 && p[1]==0x1f ? 4 : 0; break; default: break; }
            return length && length<=n ? length : 0;
        };
        auto function=[&](std::uint32_t root,std::vector<Range> parts) {
            for (const auto r:parts) {
                owners[r.begin]=root;
                auto at=r.begin;
                for (;r.end-at>=3;at+=3) { disk[at]=0x55; disk[at+1]=static_cast<std::uint8_t>(at); disk[at+2]=0x11; }
                for (;at<r.end;++at) disk[at]=0x90;
            }
            procedures[root]=parts;
        };
        function(0x1000,{{0x1000,0x1100}});                  // A: entry detours
        function(0x1200,{{0x1200,0x1400}});                  // B: interior jumps (boundaries 0x1200+3k)
        function(0x1500,{{0x1500,0x1600},{0x1700,0x1780}});  // C: a chained part
        function(0x1800,{{0x1800,0x1900}});                  // D: a 40-byte instruction at 0x1830
        disk[0x1830]=0xd6; std::fill(disk.begin()+0x1831,disk.begin()+0x1858,0x90);
        function(0x1f00,{{0x1f00,0x2100}});                  // F: an original jmp rel32 at 0x1fff, across a 4096 chunk
        disk[0x1fff]=0xe9; disk[0x2000]=0x10; disk[0x2001]=disk[0x2002]=disk[0x2003]=0;
        for (std::uint32_t at=0x2004;at<0x2100;at+=3) { disk[at]=0x55; disk[at+1]=disk[at+2]=0x22; }
        for (std::uint32_t k=0;k<300;++k) function(0x3000+0x20*k,{{0x3000+0x20*k,0x3018+0x20*k}});
        memory=disk;
        std::uint64_t cell_value=hook+0x40;
        using Reader=std::function<bool(std::uint64_t,void*,std::size_t)>;
        Reader file=[&](std::uint64_t at,void* p,std::size_t n) {
            if (at<base || at-base>size || n>size-(at-base)) return false;
            std::memcpy(p,disk.data()+(at-base),n); return true;
        };
        Reader live=[&](std::uint64_t at,void* p,std::size_t n) {
            if (at==cell_at && n==8) { std::memcpy(p,&cell_value,8); return true; }
            if (at<base || at-base>size || n>size-(at-base)) return false;
            std::memcpy(p,memory.data()+(at-base),n); return true;
        };
        // Executable: the hook memory and this image's own code.
        auto executable=[](std::uint64_t at) { return (at>=base+0x40000000 && at<base+0x40100000) || (at>=base+text && at<base+text+text_size); };
        auto classify=[&](std::uint32_t root,Detour& d) { return admit(base,root,procedures.at(root),file,live,x86,executable,d); };
        auto compare=[&] {
            const std::array<CodeSection,1> code{{{text,std::span<const std::uint8_t>(disk.data()+text,text_size)}}};
            return compare_image(base,size,code,owners,procedures,
                [&](std::uint32_t rva,void* p,std::size_t n) { return live(base+rva,p,n); },classify,file,live,x86,executable);
        };
        auto jump=[&](std::uint32_t at,std::uint64_t target) {
            memory[at]=0xe9; const auto disp=static_cast<std::int32_t>(static_cast<std::int64_t>(target-(base+at+5)));
            std::memcpy(memory.data()+at+1,&disp,4);
        };
        auto e9=[&](std::uint32_t at,std::uint32_t cover) { jump(at,hook); std::fill(memory.begin()+at+5,memory.begin()+at+cover,0x90); };
        auto far=[&](std::uint32_t at,std::uint32_t cover) {
            memory[at]=0xff; memory[at+1]=0x25; std::fill(memory.begin()+at+2,memory.begin()+at+6,0);
            std::memcpy(memory.data()+at+6,&hook,8); std::fill(memory.begin()+at+14,memory.begin()+at+cover,0x90);
        };
        auto pointer=[&](std::uint32_t at,std::uint32_t cover,std::uint64_t cell) {
            memory[at]=0xff; memory[at+1]=0x25;
            const auto disp=static_cast<std::int32_t>(static_cast<std::int64_t>(cell-(base+at+6)));
            std::memcpy(memory.data()+at+2,&disp,4); std::fill(memory.begin()+at+6,memory.begin()+at+cover,0x90);
        };
        auto accepted=[&](std::size_t entries,std::size_t interiors) {
            const auto r=compare(); return !r.reason && r.entries==entries && r.interiors==interiors;
        };
        auto refused=[&] { return compare().reason!=nullptr; };
        Detour d;
        check(accepted(0,0),"image: unchanged code");
        e9(0x1000,6); check(accepted(1,0) && classify(0x1000,d)==Admission::detour && d.span==6,"image: entry detour alone, as before");
        memory=disk; e9(0x1230,6);
        check(accepted(0,1),"image: Kiln E9 mid hook at an instruction boundary of an unrelated function");
        check(classify(0x1200,d)==Admission::refuse,"image: the function holding the interior jump is never read through the file");
        check(!selectable(Admission::refuse),"image: and never selected");
        e9(0x1000,6); check(accepted(1,1) && classify(0x1000,d)==Admission::detour && d.span==6,"image: entry detour elsewhere beside an interior jump");
        memory=disk; e9(0x1215,6); check(accepted(0,1) && classify(0x1200,d)==Admission::refuse,"image: interior jump within the first 64 bytes is not an entry detour");
        memory=disk; e9(0x1000,6); e9(0x1030,6); check(accepted(0,2) && classify(0x1000,d)==Admission::refuse,"image: entry detour plus interior jump in one function, never read through");
        memory=disk; e9(0x1231,5); check(refused(),"image: E9 at a non-boundary offset refused");
        memory=disk; far(0x1260,15); check(accepted(0,1),"image: FF 25 00000000 + absolute target (Kiln far form)");
        memory=disk; pointer(0x1260,6,cell_at); check(accepted(0,1),"image: FF 25 disp32 through an outside cell");
        memory=disk; pointer(0x1260,6,base+0x1262); check(refused(),"image: FF 25 cell inside its own span refused");
        cell_value=base+0x40200000; memory=disk; pointer(0x1260,6,cell_at); check(refused(),"image: FF 25 cell naming non-executable code refused");
        cell_value=hook+0x40;
        memory=disk; e9(0x1230,6); std::fill(memory.begin()+0x1236,memory.begin()+0x1239,0x90);
        check(refused(),"image: padding beyond the overwritten instructions refused");
        memory=disk; e9(0x1230,5); check(refused(),"image: transfer leaving part of an overwritten instruction refused");
        memory=disk; e9(0x1830,40); check(refused(),"image: interior span longer than the bound refused");
        memory=disk; e9(0x1230,6); memory[0x1235]=0xcc; check(refused(),"image: fill other than 0x90 refused");
        memory=disk; jump(0x1230,base+0x5000); memory[0x1235]=0x90; check(refused(),"image: interior jump into this image refused");
        memory=disk; jump(0x1230,base+0x40200000); memory[0x1235]=0x90; check(refused(),"image: interior jump to non-executable memory refused");
        memory=disk; e9(0x1230,6); memory[0x1300]^=1; check(refused(),"image: body byte changed beside an admitted interior jump refused");
        memory=disk; memory[0x1150]^=1; check(refused(),"image: changed byte between functions refused");
        memory=disk; e9(0x13fe,5); check(refused(),"image: transfer that does not fit its unwind range refused");
        memory=disk; e9(0x1712,6); check(accepted(0,1),"image: interior jump in a chained unwind part");
        memory=disk; jump(0x1fff,hook); memory[0x1fff]=0xe9;
        check(memory[0x1fff]==disk[0x1fff] && accepted(0,1),"image: hook over an original jmp, first change in the next comparison chunk");
        memory=disk; for (std::uint32_t k=1;k<=interior_function_limit;++k) e9(0x1200+0x30*k,6);
        check(accepted(0,interior_function_limit),"image: interior jumps per function up to the bound");
        e9(0x1200+0x30*(interior_function_limit+1),6); check(refused(),"image: one interior jump over the per-function bound refused");
        memory=disk; for (std::uint32_t k=0;k<interior_image_limit;++k) e9(0x3003+0x20*k,6);
        check(accepted(0,interior_image_limit),"image: interior jumps up to the image bound");
        e9(0x3003+0x20*interior_image_limit,6); check(refused(),"image: one interior jump over the image bound refused");
        memory=disk;
    }
    // Base relocations: the file's table applied to the cached code for the load delta, as the loader applied it.
    {
        constexpr std::uint64_t preferred=0x140000000, loaded=0x6ffffc670000, delta=loaded-preferred;
        constexpr std::uint32_t size=0x4000, text=0x1000, text_size=0x200;
        auto block=[](std::vector<std::uint8_t>& table,std::uint32_t page,std::vector<std::uint16_t> entries) {
            const auto size=static_cast<std::uint32_t>(8+2*entries.size());
            const auto at=table.size(); table.resize(at+size);
            std::memcpy(table.data()+at,&page,4); std::memcpy(table.data()+at+4,&size,4);
            std::memcpy(table.data()+at+8,entries.data(),2*entries.size());
        };
        // One function at 0x1000..0x1200 holding a 64-bit and a 32-bit absolute field; one more field in data.
        std::vector<std::uint8_t> disk(size,0xcc);
        for (std::uint32_t at=text;at+3<=text+text_size;at+=3) { disk[at]=0x55; disk[at+1]=0x01; disk[at+2]=0x02; }
        const std::uint64_t pointer=preferred+0x1234; const std::uint32_t low=0x40001000;
        std::memcpy(disk.data()+0x1010,&pointer,8); std::memcpy(disk.data()+0x1040,&low,4);
        std::vector<std::uint8_t> table;
        block(table,0x1000,{0xa010,0x3040,0x0000}); block(table,0x3000,{0xa008});
        auto relocated=[&](std::vector<std::uint8_t>& image,const std::vector<std::uint8_t>& relocations,std::uint64_t by) {
            const std::array<CodeBytes,1> code{{{text,std::span<std::uint8_t>(image.data()+text,text_size)}}};
            return relocate(relocations,size,by,code,relocations);
        };
        auto copy=disk;
        check(!relocated(copy,table,delta),"relocation table applied");
        std::uint64_t moved{}; std::uint32_t moved_low{};
        std::memcpy(&moved,copy.data()+0x1010,8); std::memcpy(&moved_low,copy.data()+0x1040,4);
        check(moved==pointer+delta && moved_low==low+static_cast<std::uint32_t>(delta),"DIR64 and HIGHLOW fields moved by the delta");
        copy[0x1010]=disk[0x1010]; copy[0x1011]=disk[0x1011]; copy[0x1012]=disk[0x1012]; copy[0x1013]=disk[0x1013];
        copy[0x1014]=disk[0x1014]; copy[0x1015]=disk[0x1015]; copy[0x1016]=disk[0x1016]; copy[0x1017]=disk[0x1017];
        copy[0x1040]=disk[0x1040]; copy[0x1041]=disk[0x1041]; copy[0x1042]=disk[0x1042]; copy[0x1043]=disk[0x1043];
        check(copy==disk,"no other byte touched (the data field lies outside the cached code)");
        copy=disk; check(!relocated(copy,{},0) && copy==disk,"not relocated: nothing applied, nothing read");
        copy=disk; check(relocated(copy,{},delta)!=nullptr,"relocated image without base relocations refused");
        auto corrupt=[&](std::vector<std::uint8_t> bad) { auto image=disk; return relocated(image,bad,delta)!=nullptr; };
        auto bad=table; bad.resize(bad.size()+4); check(corrupt(bad),"truncated block header refused");
        bad.clear(); block(bad,0x1000,{0x3011});
        const std::array<CodeBytes,1> code{{{text,std::span<std::uint8_t>(copy.data()+text,text_size)}}};
        check(relocate(bad,size,delta,code,table)!=nullptr,"relocation table: file-only relocation over a code operand refused");
        bad=table; bad.pop_back(); bad.pop_back(); check(corrupt(bad),"truncated block refused");
        bad=table; bad[4]=6; check(corrupt(bad),"block shorter than its header refused");
        bad.clear(); block(bad,0x1000,{0xa010,0xa018}); bad[4]=11; bad.resize(11);
        check(corrupt(bad),"block of odd size ending the table refused, nothing read past it");
        bad.clear(); block(bad,0x1000,{0x4010}); check(corrupt(bad),"unsupported relocation type refused");
        bad.clear(); block(bad,size,{0xa000}); check(corrupt(bad),"block page outside the image refused");
        bad.clear(); block(bad,0x3000,{0xaffc}); check(corrupt(bad),"field past the end of the image refused");
        bad.clear(); block(bad,0x1000,{0xa1fc}); check(corrupt(bad),"field across the end of a code section refused");
        bad.assign(relocation_limit+8,0); check(corrupt(bad),"table over the bound refused before it is read");
        std::vector<Section> file_sections{{{},0x1000,0x800,0x400,0x600,0x60000020},{{},0x2000,0x100,0xa00,0x200,0x42000040}};
        std::uint64_t at{};
        check(file_offset(file_sections,0x2010,0x40,at) && at==0xa10,"relocation table found in its section's raw data");
        check(!file_offset(file_sections,0x1500,0x200,at),"table beyond its section's raw data refused");
        check(!file_offset(file_sections,0x20f0,0x20,at) && !file_offset(file_sections,0x3000,8,at),"table outside a section refused");
        // End to end: the relocated loaded code is accepted, any other change is still refused.
        std::map<std::uint32_t,std::uint32_t> owners{{text,text}};
        std::map<std::uint32_t,std::vector<Range>> procedures{{text,{{text,text+text_size}}}};
        auto decode3=[](const std::uint8_t* p,std::size_t n) -> std::size_t { return n>=3 && *p==0x55 ? 3 : n ? 1 : 0; };
        auto compare=[&](const std::vector<std::uint8_t>& memory) {
            auto cached=disk;
            if (relocated(cached,table,delta)) return false;
            using Reader=std::function<bool(std::uint64_t,void*,std::size_t)>;
            Reader file=[&](std::uint64_t p,void* out,std::size_t n) { if (p<loaded||p-loaded>size||n>size-(p-loaded)) return false; std::memcpy(out,cached.data()+(p-loaded),n); return true; };
            Reader live=[&](std::uint64_t p,void* out,std::size_t n) { if (p<loaded||p-loaded>size||n>size-(p-loaded)) return false; std::memcpy(out,memory.data()+(p-loaded),n); return true; };
            auto executable=[](std::uint64_t){ return true; };
            const std::array<CodeSection,1> code{{{text,std::span<const std::uint8_t>(cached.data()+text,text_size)}}};
            return !compare_image(loaded,size,code,owners,procedures,[&](std::uint32_t rva,void* out,std::size_t n){return live(loaded+rva,out,n);},
                [&](std::uint32_t root,Detour& d){return admit(loaded,root,procedures.at(root),file,live,decode3,executable,d);},
                file,live,decode3,executable).reason;
        };
        auto memory=disk;
        const std::uint64_t moved_pointer=pointer+delta; const std::uint32_t moved_field=low+static_cast<std::uint32_t>(delta);
        std::memcpy(memory.data()+0x1010,&moved_pointer,8); std::memcpy(memory.data()+0x1040,&moved_field,4);
        check(compare(memory),"relocated image: loaded code equals the file after its relocations");
        auto changed=memory; changed[0x1100]^=1; check(!compare(changed),"relocated image: a byte no relocation explains refused");
        changed=memory; ++changed[0x1010]; check(!compare(changed),"relocated image: a relocated field holding another value refused");
        check(!compare(disk),"relocated image: relocations the loader did not apply refused");
        // A forged relocation inside a hidden original CALL would fabricate its file-backed call edge.
        // Original CALL at 1010: E8 20 00 00 00. A file-only HIGHLOW at 1011 adds 0x10000 to its rel32.
        std::vector<std::uint8_t> hidden(32,0x90);
        hidden[0]=0xe8; const std::uint32_t original_rel=0x20;
        std::memcpy(hidden.data()+1,&original_rel,4);
        std::vector<std::uint8_t> forged, trusted;
        block(forged,0x1000,{0x3011,0}); block(trusted,0x3000,{0xa008,0});
        const std::array<CodeBytes,1> hidden_code{{{0x1010,hidden}}};
        const auto reason=relocate(forged,size,0x10000,hidden_code,trusted);
        if (!reason) {
            std::uint32_t changed_rel{}; std::memcpy(&changed_rel,hidden.data()+1,4);
            std::printf("forged relocation changed original CALL rel32: 0x%x -> 0x%x\n",original_rel,changed_rel);
        }
        check(reason!=nullptr,"relocation table: a forged CALL relocation hidden by an entry detour refused");
    }
    // ShroudForge's own code patches (the native main menu's call sites): accepted exactly as recorded, nothing else.
    {
        constexpr std::uint64_t base=0x140000000, thunk=base+0x10000000;
        constexpr std::uint32_t size=0x4000, text=0x1000, text_size=0x400, site=0x1030, other=0x1230;
        std::vector<std::uint8_t> disk(size,0xcc);
        auto fill=[&](std::uint32_t begin,std::uint32_t end) { for (auto at=begin;at+3<=end;at+=3) { disk[at]=0x55; disk[at+1]=0x03; disk[at+2]=0x04; } };
        fill(0x1000,0x1100); fill(0x1200,0x1300);
        // An E8 call at an instruction boundary of each function (0x30 = 16 instructions in).
        auto call=[](std::uint64_t from,std::uint64_t to) {
            std::array<std::uint8_t,own_patch_bytes> bytes{}; bytes[0]=0xe8;
            const auto relative=static_cast<std::int32_t>(static_cast<std::int64_t>(to-(from+5))); std::memcpy(bytes.data()+1,&relative,4);
            return bytes;
        };
        const auto original_call=call(base+site,base+0x1200), menu_call=call(base+site,thunk);
        std::memcpy(disk.data()+site,original_call.data(),5); std::memcpy(disk.data()+other,call(base+other,base+0x1000).data(),5);
        std::map<std::uint32_t,std::uint32_t> owners{{0x1000,0x1000},{0x1200,0x1200}};
        std::map<std::uint32_t,std::vector<Range>> procedures{{0x1000,{{0x1000,0x1100}}},{0x1200,{{0x1200,0x1300}}}};
        auto decode=[](const std::uint8_t* p,std::size_t n) -> std::size_t {
            const std::size_t length=!n?0:*p==0x55?3:*p==0xe8||*p==0xe9?5:1; return length<=n?length:0; };
        auto memory=disk;
        using Reader=std::function<bool(std::uint64_t,void*,std::size_t)>;
        Reader file=[&](std::uint64_t at,void* p,std::size_t n) { if (at<base||at-base>size||n>size-(at-base)) return false; std::memcpy(p,disk.data()+(at-base),n); return true; };
        Reader live=[&](std::uint64_t at,void* p,std::size_t n) { if (at<base||at-base>size||n>size-(at-base)) return false; std::memcpy(p,memory.data()+(at-base),n); return true; };
        auto executable=[](std::uint64_t at) { return at>=thunk && at<thunk+0x1000; };
        std::vector<OwnPatch> own;
        auto classify=[&](std::uint32_t root,Detour& d) { return admit(base,root,procedures.at(root),file,live,decode,executable,d); };
        auto compare=[&] {
            const std::array<CodeSection,1> code{{{text,std::span<const std::uint8_t>(disk.data()+text,text_size)}}};
            return compare_image(base,size,code,owners,procedures,[&](std::uint32_t rva,void* p,std::size_t n){return live(base+rva,p,n);},
                classify,file,live,decode,executable,own);
        };
        auto record=[&](std::uint32_t rva,const std::array<std::uint8_t,own_patch_bytes>& before,const std::array<std::uint8_t,own_patch_bytes>& after,std::uint32_t n) {
            OwnPatch patch; patch.rva=rva; patch.size=n; patch.original=before; patch.replacement=after; own.push_back(patch);
        };
        Detour d;
        std::memcpy(memory.data()+site,menu_call.data(),5);
        check(compare().reason!=nullptr,"own patch: an unregistered changed E8 refused");
        record(site,original_call,menu_call,5);
        auto r=compare(); check(!r.reason && r.owns==1,"own patch: the main menu's recorded call site accepted");
        check(classify(0x1000,d)==Admission::refuse,"own patch: the function holding it is never read through the file");
        memory[site+5]^=1; check(compare().reason!=nullptr,"own patch: a byte changed right after it refused");
        memory[site+5]^=1; memory[0x1250]^=1; check(compare().reason!=nullptr,"own patch: a change elsewhere still refused");
        memory[0x1250]^=1;
        own[0].replacement[2]^=1; check(compare().reason!=nullptr,"own patch: loaded bytes other than the recorded replacement refused");
        own[0].replacement[2]^=1; own[0].original[2]^=1; check(compare().reason!=nullptr,"own patch: file bytes other than the recorded original refused");
        own[0].original[2]^=1; own[0].size=own_patch_bytes+1; check(compare().reason!=nullptr,"own patch: record over the size bound refused");
        own[0].size=5;
        std::memcpy(memory.data()+other,call(base+other,thunk).data(),5);
        check(compare().reason!=nullptr,"own patch: a second, unrecorded call site refused");
        memory=disk; check(!compare().reason && compare().owns==0,"own patch: recorded but not written (restored) is fine");
        std::memcpy(memory.data()+site,menu_call.data(),5);
        memory[0x1200]=0xe9; const auto hop=static_cast<std::int32_t>(static_cast<std::int64_t>(thunk+0x40-(base+0x1205))); std::memcpy(memory.data()+0x1201,&hop,4);
        memory[0x1205]=0x90;
        r=compare(); check(!r.reason && r.owns==1 && r.entries==1,"own patch: beside an entry detour elsewhere");
        OwnPatchRegistry registry;
        check(registry.Record(own[0]),"own registry: initial record");
        auto adjacent=own[0]; adjacent.rva+=8;
        check(registry.Record(adjacent),"own registry: disjoint record");
        auto expanded=own[0]; expanded.size=16;
        check(!registry.Record(expanded),"own registry: expanding a record across another patch refused");
        registry.Forget(adjacent.rva); registry.Record(own[0]);
        memory=disk; std::memcpy(memory.data()+site,menu_call.data(),5);
        std::promise<void> observed, restored;
        auto begin=observed.get_future(); auto done=restored.get_future();
        std::atomic<bool> comparing{}, raced{};
        std::thread restorer([&] {
            begin.wait();
            registry.Restore(site,[&] {
                raced=comparing.load();
                std::memcpy(memory.data()+site,original_call.data(),5); return true;
            });
            restored.set_value();
        });
        const auto concurrent=registry.WithPatches([&](auto patches) {
            comparing=true;
            bool first=true;
            const std::array<CodeSection,1> code{{{text,std::span<const std::uint8_t>(disk.data()+text,text_size)}}};
            auto result=compare_image(base,size,code,owners,procedures,[&](auto rva,void* p,auto n) {
                const auto ok=live(base+rva,p,n);
                if (first) {
                    first=false; observed.set_value();
                    // Before the fix, restoration completes here and the saved record no longer matches memory.
                    // With the transaction lock, restoration waits until the comparison has returned.
                    done.wait_for(std::chrono::milliseconds(100));
                }
                return ok;
            },classify,file,live,decode,executable,patches);
            comparing=false;
            return result;
        });
        restorer.join();
        check(!concurrent.reason && !raced,"own registry: restore cannot race the image comparison");
        check(!registry.Apply(own[0],[]{return false;}),"own registry: a failed install is reported");
        check(registry.WithPatches([](auto p){return p.empty();}),"own registry: failed install leaves no stale record");
        check(registry.Apply(own[0],[]{return true;}),"own registry: successful install is recorded");
        check(!registry.Restore(site,[]{return false;}),"own registry: a failed restore is reported");
        check(registry.WithPatches([](auto p){return p.size()==1;}),"own registry: failed restore keeps the active record");
        check(registry.Restore(site,[]{return true;}) && registry.WithPatches([](auto p){return p.empty();}),
            "own registry: successful restore removes its record");
    }
    // Real x86 control flow: unwind extents can contain unreachable bytes and inline data.
    {
        constexpr std::uint64_t base=0x10000000, hook=base+0x10000;
        constexpr std::uint32_t root=0x10, site=root+7;
        std::vector<std::uint8_t> disk(128,0x90), memory;
        std::map<std::uint32_t,std::uint32_t> owners{{root,root}};
        std::map<std::uint32_t,std::vector<Range>> procedures{{root,{{root,0x60}}}};
        using Reader=std::function<bool(std::uint64_t,void*,std::size_t)>;
        Reader file=[&](auto at,void* p,auto n) { if(at<base||at-base>disk.size()||n>disk.size()-(at-base))return false; std::memcpy(p,disk.data()+at-base,n);return true; };
        Reader live=[&](auto at,void* p,auto n) { if(at<base||at-base>memory.size()||n>memory.size()-(at-base))return false; std::memcpy(p,memory.data()+at-base,n);return true; };
        auto compare=[&] {
            const std::array<CodeSection,1> code{{{0,disk}}};
            return compare_image(base,128,code,owners,procedures,
                [&](auto rva,void* p,auto n){return live(base+rva,p,n);},
                [&](auto rva,Detour& d){return admit(base,rva,procedures.at(rva),file,live,real_decode,[](auto){return true;},d);},
                file,live,real_decode,[](auto){return true;});
        };
        auto patch=[&](std::uint32_t at) {
            memory=disk; memory[at]=0xe9;
            const auto disp=static_cast<std::int32_t>(hook-base-at-5);
            std::memcpy(memory.data()+at+1,&disp,4);
        };
        // EB 01 skips B8; linear decoding starts MOV eax at +2 and invents a boundary at +7.
        // The executed instruction is actually MOV rax,imm64 at +3, whose immediate includes +7..+11.
        disk[root]=0xeb; disk[root+1]=1; disk[root+2]=0xb8;
        disk[root+3]=0x48; disk[root+4]=0xb8; disk[root+13]=0xc3;
        patch(site);
        check(compare().reason!=nullptr,"x86 boundary: jump-shaped edit inside a reached MOV immediate refused");
        // EB 05 skips an invalid instruction byte (D6 in long mode) and reaches the actual hook.
        disk.assign(128,0x90); disk[root]=0xeb; disk[root+1]=5; disk[root+2]=0xd6; disk[root+20]=0xc3;
        patch(site);
        check(!compare().reason,"x86 boundary: real hook reached across skipped invalid bytes accepted");
        disk.assign(128,0x90); disk[root+64]=0xc3;
        const auto at=root+16;
        patch(at); check(!compare().reason,"x86 forms: E9 over five whole NOPs accepted");
        memory[at+5]=0xcc; check(compare().reason!=nullptr,"x86 forms: extra filler beyond the minimum span refused");
        memory=disk; memory[at]=0xff; memory[at+1]=0x25;
        std::fill(memory.begin()+at+2,memory.begin()+at+6,0);
        std::memcpy(memory.data()+at+6,&hook,8);
        check(!compare().reason,"x86 forms: FF 25 with adjacent absolute target accepted");
        std::memcpy(disk.data()+112,&hook,8); memory=disk;
        memory[at]=0xff; memory[at+1]=0x25;
        const std::int32_t cell_disp=112-at-6;
        std::memcpy(memory.data()+at+2,&cell_disp,4);
        check(!compare().reason,"x86 forms: FF 25 through an unchanged outside cell accepted");
        const std::int32_t overlap=-1;
        std::memcpy(memory.data()+at+2,&overlap,4);
        check(compare().reason!=nullptr,"x86 forms: pointer cell overlapping the transfer refused");
    }
    // A few maximum-sized NOP functions must not cause millions of decoder/reader calls for every hook.
    {
        constexpr std::uint64_t base=0x10000000;
        constexpr std::uint32_t length=procedure_limit, functions=5;
        std::vector<std::uint8_t> disk(length*functions,0x90), memory=disk;
        std::map<std::uint32_t,std::uint32_t> owners;
        std::map<std::uint32_t,std::vector<Range>> procedures;
        for (std::uint32_t i=0;i<functions;++i) {
            const auto root=i*length;
            owners[root]=root; procedures[root]={{root,root+length}};
            memory[root+length-32]=0xe9; const std::int32_t disp=0x40000000;
            std::memcpy(memory.data()+root+length-31,&disp,4);
        }
        using Reader=std::function<bool(std::uint64_t,void*,std::size_t)>;
        Reader file=[&](auto at,void* p,auto n){if(at<base||at-base>disk.size()||n>disk.size()-(at-base))return false;std::memcpy(p,disk.data()+at-base,n);return true;};
        Reader live=[&](auto at,void* p,auto n){if(at<base||at-base>memory.size()||n>memory.size()-(at-base))return false;std::memcpy(p,memory.data()+at-base,n);return true;};
        std::size_t decoded{};
        auto decode=[&](const std::uint8_t*,std::size_t n)->std::size_t {++decoded;return n?1:0;};
        const std::array<CodeSection,1> code{{{0,disk}}};
        compare_image(base,static_cast<std::uint32_t>(disk.size()),code,owners,procedures,
            [&](auto rva,void* p,auto n){return live(base+rva,p,n);},
            [&](auto root,Detour& d){return admit(base,root,procedures.at(root),file,live,decode,[](auto){return true;},d);},
            file,live,decode,[](auto){return true;});
        check(decoded<=boundary_decode_limit+interior_image_limit*interior_span_limit,
            "image work budget: repeated boundary walks stop within the image decoder budget");
    }
    std::printf("%d checks, %d failures\n",checks,failures);return failures?1:0;
}
