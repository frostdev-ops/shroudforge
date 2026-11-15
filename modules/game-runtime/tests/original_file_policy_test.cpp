#include "code-inventory/original_file_policy.h"
#include <cstdio>
#include <functional>
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
    std::printf("%d checks, %d failures\n",checks,failures);return failures?1:0;
}
