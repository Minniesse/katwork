#![no_main]

use katwork::docs::{self, DocLimits, MediaKind};
use libfuzzer_sys::fuzz_target;

// Small caps, so the fuzzer reaches the limit branches.
fn limits() -> DocLimits {
    DocLimits {
        max_input_bytes: 1 << 20,
        max_stream_bytes: 1 << 20,
        max_decoded_bytes: 8 << 20,
        max_objstm_bytes: 1 << 20,
        max_pdf_objects: 10_000,
        max_pages: 16,
        max_content_ops: 100_000,
        ..DocLimits::default()
    }
}

fuzz_target!(|data: &[u8]| {
    if let Ok(extracted) = docs::extract(MediaKind::Pdf, data, &limits()) {
        for block in &extracted.blocks {
            assert!(!block.text.contains('\0'));
            let len = block.text.chars().count() as u32;
            for line in &block.lines {
                assert!(line.start <= line.end && line.end <= len);
            }
        }
    }
});
