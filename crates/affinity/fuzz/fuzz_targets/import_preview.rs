#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Exercise the bounded preview PNG decoder and document mapping, not unrelated formats.
    if photocraft_affinity::is_affinity(data) {
        let _ = photocraft_io::import("fuzz.af", data);
    }
});
