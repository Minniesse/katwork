#![no_main]

use katwork::docs::{self, DocLimits, Locator, MediaKind};
use libfuzzer_sys::fuzz_target;

// Small caps, so the fuzzer reaches the limit branches.
fn limits() -> DocLimits {
    DocLimits {
        max_input_bytes: 1 << 20,
        max_unzipped_bytes: 4 << 20,
        max_zip_entries: 64,
        max_xml_depth: 32,
        max_xml_elements: 100_000,
        max_cells: 10_000,
        max_shared_strings: 10_000,
        max_blocks: 10_000,
        ..DocLimits::default()
    }
}

fuzz_target!(|data: &[u8]| {
    if let Ok(extracted) = docs::extract(MediaKind::Xlsx, data, &limits()) {
        for block in &extracted.blocks {
            assert!(!block.text.contains('\0'));
            assert!(matches!(block.locator, Locator::XlsxCell { .. }));
            // Every block is a cell, and the cell says what the block says about being hidden.
            let cell = block.cell.as_ref().expect("an XLSX block is a cell");
            assert_eq!(cell.hidden, block.hidden.is_some());
            for text in [&cell.cached, &cell.displayed, &cell.formula, &cell.number_format] {
                assert!(!text.as_deref().unwrap_or_default().contains('\0'));
            }
        }
    }
});
