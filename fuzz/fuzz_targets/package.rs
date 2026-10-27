#![no_main]

use katwork::docs::{DocLimits, package::open_package};
use libfuzzer_sys::fuzz_target;

// Small caps, so the fuzzer reaches the limit branches.
fn limits() -> DocLimits {
    DocLimits {
        max_input_bytes: 1 << 20,
        max_unzipped_bytes: 4 << 20,
        max_zip_entries: 64,
        max_xml_depth: 32,
        ..DocLimits::default()
    }
}

fuzz_target!(|data: &[u8]| {
    let limits = limits();
    if let Ok(package) = open_package(data, &limits) {
        // A package the guard accepts survives its own stored copy unchanged.
        let stored = package.to_stored_zip();
        let again = open_package(&stored, &limits).expect("the stored copy opens");
        assert_eq!(again, package);
        assert_eq!(again.to_stored_zip(), stored);
    }
});
