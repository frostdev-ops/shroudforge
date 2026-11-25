// SPDX-License-Identifier: MIT
//! Kiln layer: `shroudforge_host_skip_packages` (kiln/abi/host_skip.h) takes a host's list of packages that are off
//! for this start; `modules/modloader/kiln_host_skip.rs` tests what the list does.

use super::shroudforge_host_skip_packages;
use shroudforge_package::kiln_host_skip::is_skipped;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

#[test]
fn the_export_takes_a_host_list_one_id_per_line() {
    let ids = wide("kiln-export-own\nkiln-export-second");
    assert!(unsafe { shroudforge_host_skip_packages(ids.as_ptr()) });
    assert!(is_skipped("kiln-export-own") && is_skipped("kiln-export-second"));
    assert!(!is_skipped("kiln-export-other"));
}

#[test]
fn the_export_refuses_what_is_no_list_of_ids() {
    assert!(!unsafe { shroudforge_host_skip_packages(std::ptr::null()) });
    assert!(!unsafe { shroudforge_host_skip_packages(wide("").as_ptr()) });
    assert!(!unsafe {
        shroudforge_host_skip_packages(wide("kiln-export-valid\n../escape").as_ptr())
    });
    assert!(
        !is_skipped("kiln-export-valid"),
        "an invalid list adds nothing"
    );
}
