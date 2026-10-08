#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = photocraft_affinity::inspect(data);
    let _ = photocraft_affinity::preview(data);
});
