#include "code-inventory/original_file_policy.h"
#include <cstdio>
#include <functional>
#include <map>
#include <string>
using namespace OriginalFile;
int checks{}, failures{};
void check(bool ok, const char* name) { ++checks; if(!ok) { ++failures; std::printf("FAIL %s\n",name); } }
int main() {
    Layout a; a.preferred=a.loaded=0x100000; a.size=4096; a.machine=0x8664; a.count=1;
    a.timestamp=1; a.checksum=2; a.sections.push_back({{},0,256,512,256,0x60000020});
    check(!identity(a,a),"identical headers");
    auto b=a; b.sections[0].raw=1024; check(identity(a,b),"changed section layout");
    b=a; ++b.timestamp; check(identity(a,b),"changed timestamp");
    b=a; ++b.checksum; check(identity(a,b),"changed checksum");
    b=a; ++b.machine; check(identity(a,b),"changed machine");
    b=a; ++b.count; check(identity(a,b),"changed section count");
    b=a; b.loaded+=4096; check(identity(b,a),"relocated base");
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
    std::printf("%d checks, %d failures\n",checks,failures);return failures?1:0;
}
