//! The font bundle (R14): exactly the files `FONTS.md` lists, each with the SHA-256 it records.
use std::{collections::BTreeMap, path::Path};

use sha2::{Digest, Sha256};

const FONTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts");

/// The bundle the plan names: Sarabun's four static styles and Noto Sans Thai Regular and Bold.
const BUNDLE: [&str; 6] = [
    "NotoSansThai-Bold.ttf",
    "NotoSansThai-Regular.ttf",
    "Sarabun-Bold.ttf",
    "Sarabun-BoldItalic.ttf",
    "Sarabun-Italic.ttf",
    "Sarabun-Regular.ttf",
];

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `FONTS.md`'s table: each row's file name and SHA-256.
fn manifest() -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(Path::new(FONTS).join("FONTS.md")).unwrap();
    let mut rows = BTreeMap::new();
    for line in text.lines().filter(|line| line.starts_with("| `")) {
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        let (file, hash) = (cells[1].trim_matches('`'), cells[2].trim_matches('`'));
        assert_eq!(cells[3], "OFL-1.1", "{file}'s licence");
        assert!(
            rows.insert(file.to_owned(), hash.to_owned()).is_none(),
            "{file} is listed once"
        );
    }
    rows
}

fn is_font(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        ".ttf", ".otf", ".ttc", ".otc", ".woff", ".woff2", ".pfb", ".pfa",
    ]
    .iter()
    .any(|extension| lower.ends_with(extension))
}

#[test]
fn font_bundle_hashes_match_manifest() {
    let listed = manifest();
    assert_eq!(
        listed.keys().map(String::as_str).collect::<Vec<_>>(),
        BUNDLE,
        "FONTS.md lists exactly the bundle"
    );
    let mut present = BTreeMap::new();
    for entry in std::fs::read_dir(FONTS).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        assert!(
            entry.file_type().unwrap().is_file(),
            "{name} is a plain file"
        );
        match name.as_str() {
            "FONTS.md" | "OFL.txt" => {}
            _ => {
                assert!(is_font(&name), "{name} is neither a font nor the manifest");
                present.insert(name, sha256(&std::fs::read(entry.path()).unwrap()));
            }
        }
    }
    assert_eq!(present, listed, "every font file is listed with its hash");
    let licence = std::fs::read_to_string(Path::new(FONTS).join("OFL.txt")).unwrap();
    assert!(licence.contains("Copyright 2018 The Sarabun Project Authors"));
    assert!(licence.contains("Copyright 2022 The Noto Project Authors"));
    assert!(licence.contains("SIL OPEN FONT LICENSE Version 1.1"));
}
