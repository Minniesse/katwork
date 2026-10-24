#![no_main]

use katwork::docs::{self, DocLimits, MediaKind};
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
    if let Ok(extracted) = docs::extract(MediaKind::Docx, data, &limits()) {
        for block in &extracted.blocks {
            assert!(!block.text.contains('\0'));
            assert!(block.hidden.is_none());
        }
    }
});
