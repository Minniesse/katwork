use std::time::Duration;

/// Caps on one document read or render (the SP2a plan's Global Constraints).
#[derive(Debug, Clone, PartialEq)]
pub struct DocLimits {
    pub max_input_bytes: u64,
    /// The sum over a package of the bytes actually inflated, never the declared sizes.
    pub max_unzipped_bytes: u64,
    /// Inflated bytes ÷ compressed size, for entries above 1 MiB inflated.
    pub max_entry_ratio: u64,
    pub max_zip_entries: u32,
    pub max_xml_depth: u32,
    /// Non-empty XLSX cells.
    pub max_cells: u32,
    pub max_shared_strings: u32,
    pub max_pages: u32,
    /// One decoded PDF stream of any kind.
    pub max_stream_bytes: u64,
    /// One budget per PDF: load-time object-stream reservations plus every stream the pre-scan
    /// decodes.
    pub max_decoded_bytes: u64,
    /// One decoded object stream at load time; each reserves this much of `max_decoded_bytes`.
    pub max_objstm_bytes: u64,
    pub max_object_streams: u32,
    pub max_pdf_objects: u32,
    pub max_xobject_depth: u32,
    pub max_page_tree_depth: u32,
    pub max_content_ops: u64,
    /// Per extraction.
    pub max_blocks: u32,
    pub max_image_pixels: u64,
    pub max_output_bytes: u64,
    pub max_output_pages: u32,
    /// Text below this effective size is hidden.
    pub min_visible_pt: f32,
    /// Text whose WCAG 2 contrast ratio against what lies beneath it is below this is hidden.
    pub min_contrast: f32,
    /// PDF text painted with an alpha below this is hidden.
    pub min_text_alpha: f32,
    /// A pool job's cap, counted from when a worker picks the job up.
    pub call_timeout: Duration,
    /// How long `DocPool::wedged()` must stay true before the watchdog exits the process.
    pub wedge_exit_after: Duration,
    /// `read_file`'s extraction cache.
    pub cache_bytes: u64,
}

impl Default for DocLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 26_214_400,
            max_unzipped_bytes: 209_715_200,
            max_entry_ratio: 100,
            max_zip_entries: 2_000,
            max_xml_depth: 256,
            max_cells: 250_000,
            max_shared_strings: 1_000_000,
            max_pages: 500,
            max_stream_bytes: 16_777_216,
            max_decoded_bytes: 268_435_456,
            max_objstm_bytes: 2_097_152,
            max_object_streams: 64,
            max_pdf_objects: 500_000,
            max_xobject_depth: 8,
            max_page_tree_depth: 32,
            max_content_ops: 2_000_000,
            max_blocks: 50_000,
            max_image_pixels: 40_000_000,
            max_output_bytes: 52_428_800,
            max_output_pages: 200,
            min_visible_pt: 4.0,
            min_contrast: 1.2,
            min_text_alpha: 0.1,
            call_timeout: Duration::from_secs(30),
            wedge_exit_after: Duration::from_secs(30),
            cache_bytes: 67_108_864,
        }
    }
}
