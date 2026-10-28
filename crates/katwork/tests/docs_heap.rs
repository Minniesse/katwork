//! The read's memory budget, and the peak-heap harness: every flood the DOCX reader's reviews
//! measured, generated here (never committed) and read inside a job scope that counts its live
//! bytes. Each is refused or read within `max_read_bytes` and the call timeout.
//!
//! The floods run at a twentieth of the default element cap here, under that scale's budget and
//! under one most of them pass. To read them at the default caps and print each read's peak, and
//! under budgets from 64 MiB to 1 GiB how far each goes past its budget, build in release:
//! `cargo test --release -p katwork --test docs_heap -- --ignored --nocapture`
#[path = "support/ooxml.rs"]
mod ooxml;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use katwork::{
    docs::{self, DocError, DocLimits, DocPool, Extracted, Locator, MediaKind, deadline, heap},
    ids::WorkspaceId,
};
use ooxml::*;
use tokio_util::sync::CancellationToken;

#[global_allocator]
static ALLOCATOR: heap::ReadHeap = heap::ReadHeap;

#[test]
fn reads_count_their_own_bytes() {
    assert!(heap::installed());
    // Zeroed, plain and grown allocations are each counted.
    let (_, peak) = heap::measure(|| std::hint::black_box(vec![0u8; 1 << 20]));
    assert!((1 << 20..2 << 20).contains(&peak), "{peak}");
    let (_, peak) = heap::measure(|| std::hint::black_box(Vec::<u8>::with_capacity(1 << 20)));
    assert!((1 << 20..2 << 20).contains(&peak), "{peak}");
    let (_, peak) = heap::measure(|| {
        let mut grown = std::hint::black_box(Vec::<u8>::with_capacity(1 << 10));
        grown.reserve_exact(4 << 20);
        grown
    });
    assert!((4 << 20..5 << 20).contains(&peak), "{peak}");
    // What is freed no longer counts: eight 1 MiB buffers one after another peak at one.
    let (_, peak) = heap::measure(|| {
        for _ in 0..8 {
            drop(std::hint::black_box(vec![1u8; 1 << 20]));
        }
    });
    assert!((1 << 20..2 << 20).contains(&peak), "{peak}");
    // What a count frees that it did not allocate lowers it, never below nothing.
    let held = vec![0u8; 1 << 20];
    let (_, peak) = heap::measure(|| drop(held));
    assert_eq!(peak, 0);
}

/// A job past its `max_read_bytes` stops at its next `deadline::check`.
#[test]
fn a_read_past_its_budget_is_refused() {
    let paragraph = para("", &run("", &"x".repeat(4 << 20)));
    let bytes = stored(&Docx::new(&paragraph));
    let within = |max_read_bytes: u64| {
        deadline::scope(
            Instant::now() + Duration::from_secs(60),
            None,
            max_read_bytes,
            || docs::extract(MediaKind::Docx, &bytes, &DocLimits::default()).map(drop),
        )
    };
    assert_eq!(within(8 << 20), Err(DocError::TooComplex("memory")));
    assert_eq!(within(64 << 20), Ok(()));
}

/// The budget holds to a read's last step: the blocks made from its visible text count too.
/// This read passes every check while it walks the part, then copies 8 MiB of text into blocks.
#[test]
fn the_blocks_a_read_makes_count_toward_its_budget() {
    let text = "\u{1F600}".repeat(2 << 20);
    let bytes = stored(&Docx::new(&para("", &run("", &text))));
    let within = |mib: u64| {
        let limits = DocLimits {
            max_read_bytes: mib << 20,
            ..DocLimits::default()
        };
        counted(&bytes, &limits)
            .0
            .map(|extracted| extracted.blocks.len())
    };
    assert_eq!(within(28), Err(DocError::TooComplex("memory")));
    assert_eq!(within(40), Ok(1_399));
}

/// A read past its budget when its blocks begin is refused there, before it copies its text into
/// them. Its one paragraph, a single grapheme of 3 MB (an `e` and 1,500,000 combining accents)
/// and one more character, splits into two pieces with no check of its own, and holds its text in
/// room for twice its length; its peak comes as the copy ends, with that room still held. The
/// peak less half again the text's length lies below what it holds when its blocks begin and
/// above what it holds before and after.
#[test]
fn a_read_past_its_budget_before_its_blocks_is_refused_there() {
    let paragraph = format!(
        "<w:p>{}{}</w:p>",
        run("", &format!("e{}", "\u{301}".repeat(1_500_000))),
        run("", "b")
    );
    let bytes = stored(&Docx::new(&paragraph));
    let limits = DocLimits::default();
    let (result, peak, _) = counted(&bytes, &limits);
    let extracted = result.unwrap();
    assert_eq!(extracted.blocks.len(), 2);
    let text: u64 = extracted
        .blocks
        .iter()
        .map(|block| block.text.len() as u64)
        .sum();
    let limits = DocLimits {
        max_read_bytes: peak - text * 3 / 2,
        ..limits
    };
    assert_eq!(
        counted(&bytes, &limits).0.map(drop),
        Err(DocError::TooComplex("memory"))
    );
}

/// A read whose blocks pass its budget only as the last of them are made is refused: the check
/// after its last block sees them all. Its 32,800 one-character paragraphs double their blocks'
/// room after the last check of the loop that makes them; its budget, that room, is less than the
/// read holds at the end and more than it held at any check before.
#[test]
fn a_read_past_its_budget_with_its_last_blocks_is_refused() {
    let bytes = stored(&Docx::new(&para("", &run("", "x")).repeat(32_800)));
    let limits = DocLimits::default();
    let extracted = counted(&bytes, &limits).0.unwrap();
    assert_eq!(extracted.blocks.len(), 32_800);
    let limits = DocLimits {
        max_read_bytes: (extracted.blocks.capacity() * std::mem::size_of::<docs::Block>()) as u64,
        ..limits
    };
    assert_eq!(
        counted(&bytes, &limits).0.map(drop),
        Err(DocError::TooComplex("memory"))
    );
}

/// Unread content between paragraphs warns the paragraph after it once, however much of it stands
/// there: a flood of it costs what its part does.
#[test]
fn unread_content_between_paragraphs_costs_what_its_part_does() {
    let unread = r#"<x:a xmlns:x="urn:example:unknown" x:b="1"/>"#.repeat(200_000);
    let bytes = stored(&Docx::new(&format!(
        "{unread}{}",
        para("", &run("", "after"))
    )));
    let (result, peak, _) = counted(&bytes, &DocLimits::default());
    assert_eq!(result.unwrap().warnings.len(), 1);
    assert!(peak < 3 * bytes.len() as u64 + (4 << 20), "{peak}");
}

#[tokio::test]
async fn the_pool_holds_each_job_to_its_budget() {
    let pool = Arc::new(
        DocPool::new(
            1,
            DocLimits {
                max_read_bytes: 8 << 20,
                ..DocLimits::default()
            },
        )
        .unwrap(),
    );
    let run = |megabytes: usize| {
        let pool = pool.clone();
        async move {
            pool.run(
                WorkspaceId::new(),
                &CancellationToken::new(),
                None,
                move |_| {
                    let held = std::hint::black_box(vec![1u8; megabytes << 20]);
                    deadline::check()?;
                    Ok(held.len())
                },
            )
            .await
        }
    };
    assert_eq!(run(16).await, Err(DocError::TooComplex("memory")));
    assert_eq!(run(4).await, Ok(4 << 20));
}

/// `docx` as a stored zip, so a large repetitive part passes the ratio cap.
fn stored(docx: &Docx) -> Vec<u8> {
    let entries = docx.entries();
    zip(
        &entries
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
            .collect::<Vec<_>>(),
        false,
    )
}

/// `docx` deflated, each large part padded after its root with a comment of random hex, so the
/// package passes the ratio cap and fits the input cap.
fn packed(docx: &Docx) -> Vec<u8> {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let entries: Vec<(String, Vec<u8>)> = docx
        .entries()
        .into_iter()
        .map(|(name, mut bytes)| {
            if bytes.len() > 1 << 20 {
                let target = bytes.len() + bytes.len() / 40;
                bytes.extend_from_slice(b"<!--");
                while bytes.len() < target {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    bytes.extend_from_slice(format!("{state:016x}").as_bytes());
                }
                bytes.extend_from_slice(b"-->");
            }
            (name, bytes)
        })
        .collect();
    zip(
        &entries
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
            .collect::<Vec<_>>(),
        true,
    )
}

/// One flood the reviews measured, made for an element cap.
struct Flood {
    name: &'static str,
    build: fn(u64) -> Vec<u8>,
}

/// Every table style region.
const REGIONS: [&str; 13] = [
    "wholeTable",
    "band1Horz",
    "band2Horz",
    "band1Vert",
    "band2Vert",
    "firstCol",
    "lastCol",
    "firstRow",
    "lastRow",
    "nwCell",
    "neCell",
    "swCell",
    "seCell",
];

/// `unit` repeated to fill `elements` less `overhead`, `each` elements a unit.
fn fill(unit: &str, each: u64, elements: u64, overhead: u64) -> String {
    unit.repeat((elements.saturating_sub(overhead) / each) as usize)
}

fn floods() -> Vec<Flood> {
    vec![
        Flood {
            name: "table paragraphs in one cell",
            build: |elements| {
                let paras = fill("<w:p><w:r><w:t>x</w:t></w:r></w:p>", 3, elements, 20);
                packed(&Docx::new(&format!(
                    "<w:tbl><w:tr><w:tc>{paras}</w:tc></w:tr></w:tbl>"
                )))
            },
        },
        Flood {
            name: "cells 80 tables deep",
            build: |elements| {
                let (open, close) = (
                    "<w:tbl><w:tr><w:tc>".repeat(79),
                    "</w:tc></w:tr></w:tbl>".repeat(79),
                );
                let cells = fill(
                    "<w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc>",
                    4,
                    elements,
                    300,
                );
                packed(&Docx::new(&format!(
                    "{open}<w:tbl><w:tr>{cells}</w:tr></w:tbl>{close}"
                )))
            },
        },
        Flood {
            name: "two-colour shading 82 cells deep",
            build: |_| {
                let shaded = r#"<w:tbl><w:tr><w:tc><w:tcPr><w:shd w:val="pct50" w:color="123456" w:themeColor="accent1" w:fill="auto"/></w:tcPr>"#;
                Docx::new(&format!(
                    r#"{}<w:p><w:r><w:rPr><w:color w:val="ABCDEF"/></w:rPr><w:t>x</w:t></w:r></w:p>{}"#,
                    shaded.repeat(82),
                    "</w:tc></w:tr></w:tbl>".repeat(82)
                ))
                .build()
            },
        },
        Flood {
            name: "a 256-byte part name on every block and warning",
            build: |elements| {
                let paras = fill(
                    r#"<w:p><w:r><w:t>a</w:t></w:r><w:r><w:rPr><w:vanish/></w:rPr><w:t>b</w:t></w:r></w:p>"#,
                    8,
                    elements,
                    20,
                );
                let name = format!("word/{}.xml", "n".repeat(256 - 9));
                packed(&Docx::new(&paras).main_part(&name))
            },
        },
        Flood {
            name: "a 256-byte default character style id on 65,536 runs",
            build: |_| {
                let id = "s".repeat(256);
                packed(
                    &Docx::new(&format!(
                        "<w:p>{}</w:p>",
                        "<w:r><w:t>x</w:t></w:r>".repeat(65_536)
                    ))
                    .styles(&format!(
                        r#"<w:style w:type="character" w:default="1" w:styleId="{id}"><w:rPr><w:color w:val="000000"/></w:rPr></w:style>"#
                    )),
                )
            },
        },
        Flood {
            name: "a 4 MB style id",
            build: |_| {
                let id = "s".repeat(4 << 20);
                packed(&Docx::new(&para("", &run("", "x"))).styles(&format!(
                    r#"<w:style w:type="character" w:default="1" w:styleId="{id}"/>"#
                )))
            },
        },
        Flood {
            name: "one element with a million attributes",
            build: |_| {
                let attributes: String = (0..1_000_000).map(|at| format!(" a{at}=\"\"")).collect();
                packed(&Docx::new(&format!(
                    r#"<w:p><w:pPr><w:shd w:val="clear" w:fill="FFFFFF"{attributes}/></w:pPr></w:p>"#
                )))
            },
        },
        Flood {
            name: "table styles of 13 regions, each used",
            build: |elements| {
                let regions: String = REGIONS
                    .iter()
                    .map(|kind| format!(r#"<w:tblStylePr w:type="{kind}"/>"#))
                    .collect();
                let count = (elements / 2 / 14).min(65_536);
                let styles: String = (0..count)
                    .map(|at| {
                        format!(r#"<w:style w:type="table" w:styleId="t{at}">{regions}</w:style>"#)
                    })
                    .collect();
                let tables: String = (0..count)
                    .map(|at| {
                        format!(
                            r#"<w:tbl><w:tblPr><w:tblStyle w:val="t{at}"/></w:tblPr><w:tr><w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#
                        )
                    })
                    .collect();
                packed(&Docx::new(&tables).styles(&styles))
            },
        },
        Flood {
            name: "table styles of 13 shaded regions, each used",
            build: |elements| {
                let regions: String = REGIONS
                .iter()
                .map(|kind| {
                    format!(
                        r#"<w:tblStylePr w:type="{kind}"><w:rPr><w:color w:val="000000"/></w:rPr><w:tcPr><w:shd w:val="clear" w:fill="FFFFFF"/></w:tcPr></w:tblStylePr>"#
                    )
                })
                .collect();
                let count = (elements / 2 / 66).min(65_536);
                let styles: String = (0..count)
                    .map(|at| {
                        format!(r#"<w:style w:type="table" w:styleId="t{at}">{regions}</w:style>"#)
                    })
                    .collect();
                let tables: String = (0..count)
                    .map(|at| {
                        format!(
                            r#"<w:tbl><w:tblPr><w:tblStyle w:val="t{at}"/></w:tblPr><w:tr><w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#
                        )
                    })
                    .collect();
                packed(&Docx::new(&tables).styles(&styles))
            },
        },
        Flood {
            name: "table styles of 13 shaded regions, each used, every reading open",
            build: |elements| {
                let regions: String = REGIONS
                    .iter()
                    .map(|kind| {
                        format!(
                            r#"<w:tblStylePr w:type="{kind}"><w:tcPr><w:shd w:val="clear" w:fill="FFFFFF"/></w:tcPr></w:tblStylePr>"#
                        )
                    })
                    .collect();
                // 40 elements a style, 8 a table.
                let count = (elements.saturating_sub(40) / 48).min(65_000);
                let styles: String = (0..count)
                    .map(|at| {
                        format!(r#"<w:style w:type="table" w:styleId="t{at}">{regions}</w:style>"#)
                    })
                    .collect();
                // A style id defined twice and a property written twice open four readings.
                let twice = concat!(
                    r#"<w:style w:type="paragraph" w:styleId="P"><w:rPr><w:sz w:val="20"/></w:rPr></w:style>"#,
                    r#"<w:style w:type="paragraph" w:styleId="P"><w:rPr><w:sz w:val="22"/></w:rPr></w:style>"#
                );
                let tables: String = (0..count)
                    .map(|at| {
                        format!(
                            r#"<w:tbl><w:tblPr><w:tblStyle w:val="t{at}"/></w:tblPr><w:tr><w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#
                        )
                    })
                    .collect();
                let first = para("", &run(r#"<w:sz w:val="20"/><w:sz w:val="22"/>"#, "x"));
                packed(&Docx::new(&format!("{first}{tables}")).styles(&format!("{twice}{styles}")))
            },
        },
        Flood {
            name: "table styles of 13 shaded regions on a base read four ways",
            build: |elements| {
                let regions: String = REGIONS
                    .iter()
                    .map(|kind| {
                        format!(
                            r#"<w:tblStylePr w:type="{kind}"><w:tcPr><w:shd w:val="clear" w:fill="FFFFFF"/></w:tcPr></w:tblStylePr>"#
                        )
                    })
                    .collect();
                // 41 elements a style, 8 a table.
                let count = (elements.saturating_sub(40) / 49).min(65_000);
                // Their base is defined twice, each definition writing a size twice: each style
                // built on it differs under all four readings, and none is held once.
                let base = concat!(
                    r#"<w:style w:type="table" w:styleId="B"><w:rPr><w:sz w:val="20"/><w:sz w:val="22"/></w:rPr></w:style>"#,
                    r#"<w:style w:type="table" w:styleId="B"><w:rPr><w:sz w:val="24"/><w:sz w:val="26"/></w:rPr></w:style>"#
                );
                let styles: String = (0..count)
                    .map(|at| {
                        format!(
                            r#"<w:style w:type="table" w:styleId="t{at}"><w:basedOn w:val="B"/>{regions}</w:style>"#
                        )
                    })
                    .collect();
                let tables: String = (0..count)
                    .map(|at| {
                        format!(
                            r#"<w:tbl><w:tblPr><w:tblStyle w:val="t{at}"/></w:tblPr><w:tr><w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#
                        )
                    })
                    .collect();
                packed(&Docx::new(&tables).styles(&format!("{base}{styles}")))
            },
        },
        Flood {
            name: "distinct 250-byte style ids, all named in one style",
            build: |elements| {
                // As many as the inflation cap holds.
                let limits = DocLimits::default();
                let unzipped = limits.max_unzipped_bytes / (limits.max_xml_elements / elements);
                let count = (unzipped * 95 / 100 / 280).min(elements.saturating_sub(20));
                let ids: String = (0..count)
                    .map(|at| format!(r#"<w:rStyle w:val="{at:0>250}"/>"#))
                    .collect();
                packed(&Docx::new(&para("", &run("", "x"))).styles(&format!(
                    r#"<w:style w:type="character" w:styleId="c"><w:rPr>{ids}</w:rPr></w:style>"#
                )))
            },
        },
        Flood {
            name: "VML shape types with distinct ids",
            build: |elements| {
                let types: String = (0..elements.saturating_sub(40))
                    .map(|at| format!(r#"<v:shapetype id="t{at}"/>"#))
                    .collect();
                packed(&Docx::new(&format!(
                    "<w:p><w:r><w:pict>{types}</w:pict></w:r></w:p>"
                )))
            },
        },
        Flood {
            name: "relationships past the element cap in the root relationships",
            build: |elements| {
                let relationships: String = (0..elements)
                    .map(|at| format!(r#"<Relationship Id="r{at}" Type="urn:x" Target="t{at}"/>"#))
                    .collect();
                packed(&Docx::new(&para("", &run("", "x"))).root_rels(&format!(
                    r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>{relationships}"#
                )))
            },
        },
        Flood {
            name: "a text box in every cell",
            build: |elements| {
                let boxed = r#"<w:tc><w:p><w:r><w:drawing><wp:inline><a:graphic><a:graphicData><wps:wsp><wps:spPr><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill></wps:spPr><wps:txbx><w:txbxContent><w:p><w:r><w:t>x</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p></w:tc>"#;
                let cells = fill(boxed, 18, elements, 20);
                packed(&Docx::new(&format!("<w:tbl><w:tr>{cells}</w:tr></w:tbl>")))
            },
        },
        Flood {
            name: "runs, 65,536 to a paragraph",
            build: |elements| {
                let paragraph = format!("<w:p>{}</w:p>", "<w:r><w:t>x</w:t></w:r>".repeat(65_535));
                let count = (elements / (65_535 * 2 + 1)).max(1);
                packed(&Docx::new(&paragraph.repeat(count as usize)))
            },
        },
        Flood {
            name: "one paragraph of text the size of the inflation cap",
            build: |elements| {
                // 190 MB at the default cap, scaled with it.
                let len = (190 << 20) / (DocLimits::default().max_xml_elements / elements);
                let text = "x".repeat(len as usize);
                packed(&Docx::new(&para("", &run("", &text))))
            },
        },
        Flood {
            name: "the same text after a character in its run",
            build: |elements| {
                // The run copies the text onto its character while the node is still held.
                let len = (190 << 20) / (DocLimits::default().max_xml_elements / elements);
                let text = "x".repeat(len as usize);
                packed(&Docx::new(&format!(
                    "<w:p><w:r><w:t>a</w:t><w:t>{text}</w:t></w:r></w:p>"
                )))
            },
        },
        Flood {
            name: "the same text in three nodes of one run, its room doubled",
            build: |elements| {
                // The second node doubles the room of the first; the third doubles it again
                // while the node is held.
                let scale = DocLimits::default().max_xml_elements / elements;
                let first = "x".repeat(((94 << 20) / scale) as usize);
                let last = "x".repeat(((95 << 20) / scale) as usize);
                packed(&Docx::new(&format!(
                    "<w:p><w:r><w:t>{first}</w:t><w:t>x</w:t><w:t>{last}</w:t></w:r></w:p>"
                )))
            },
        },
        Flood {
            name: "every reading open, coloured runs in cells 40 tables deep",
            build: |elements| {
                let (open, close) = (
                    "<w:tbl><w:tblPr><w:tblStyle w:val=\"Bands\"/></w:tblPr><w:tr><w:tc>"
                        .repeat(40),
                    "</w:tc></w:tr></w:tbl>".repeat(40),
                );
                let cells = fill(
                    r#"<w:tc><w:p><w:r><w:rPr><w:sz w:val="20"/><w:sz w:val="22"/><w:color w:val="000000"/></w:rPr><w:t>x</w:t></w:r></w:p></w:tc>"#,
                    8,
                    elements,
                    400,
                );
                let styles = concat!(
                    r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:rPr><w:sz w:val="22"/></w:rPr></w:style>"#,
                    r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:rPr><w:sz w:val="24"/></w:rPr></w:style>"#,
                    r#"<w:style w:type="table" w:styleId="Bands"><w:tblStylePr w:type="band1Horz"><w:tcPr><w:shd w:val="pct50" w:color="123456" w:fill="ABCDEF"/></w:tcPr></w:tblStylePr></w:style>"#
                );
                packed(
                    &Docx::new(&format!("{open}<w:tbl><w:tr>{cells}</w:tr></w:tbl>{close}"))
                        .styles(styles),
                )
            },
        },
        Flood {
            name: "nested cells, a long part name and banded styles together",
            build: |elements| {
                let (open, close) = (
                    "<w:tbl><w:tblPr><w:tblStyle w:val=\"Bands\"/></w:tblPr><w:tr><w:tc>"
                        .repeat(40),
                    "</w:tc></w:tr></w:tbl>".repeat(40),
                );
                let cells = fill(
                    r#"<w:tc><w:p><w:r><w:t>a</w:t></w:r><w:r><w:rPr><w:vanish/></w:rPr><w:t>b</w:t></w:r></w:p></w:tc>"#,
                    8,
                    elements,
                    400,
                );
                let styles = r#"<w:style w:type="table" w:styleId="Bands"><w:tblStylePr w:type="band1Horz"><w:tcPr><w:shd w:val="pct50" w:color="123456" w:fill="ABCDEF"/></w:tcPr></w:tblStylePr></w:style>"#;
                // Its relationships part's name, 11 bytes longer, is the longest allowed.
                let name = format!("word/{}.xml", "n".repeat(256 - 11 - 9));
                packed(
                    &Docx::new(&format!("{open}<w:tbl><w:tr>{cells}</w:tr></w:tbl>{close}"))
                        .main_part(&name)
                        .styles(styles),
                )
            },
        },
    ]
}

/// A read as the pool runs it, its peak live bytes and how long it took.
fn counted(bytes: &[u8], limits: &DocLimits) -> (Result<Extracted, DocError>, u64, Duration) {
    let started = Instant::now();
    let (result, peak) = heap::measure(|| {
        deadline::scope(
            started + limits.call_timeout,
            None,
            limits.max_read_bytes,
            || docs::extract(MediaKind::Docx, bytes, limits),
        )
    });
    (result, peak, started.elapsed())
}

/// The default caps, scaled down by `scale` where they bound the size of a read.
fn scaled(scale: u64) -> DocLimits {
    let limits = DocLimits::default();
    DocLimits {
        max_xml_elements: limits.max_xml_elements / scale,
        max_read_bytes: limits.max_read_bytes / scale,
        max_unzipped_bytes: limits.max_unzipped_bytes / scale,
        ..limits
    }
}

/// Reads every flood made for `limits`, each refused or read within its budget; returns the most
/// any read went past its budget.
fn read_floods(limits: &DocLimits, print: bool) -> u64 {
    let mut past = 0;
    for flood in floods() {
        let bytes = (flood.build)(limits.max_xml_elements);
        let (result, peak, took) = counted(&bytes, limits);
        let outcome = match &result {
            Ok(extracted) => format!(
                "read ({} blocks, {} warnings)",
                extracted.blocks.len(),
                extracted.warnings.len()
            ),
            Err(error) => format!("{error:?}"),
        };
        if print {
            println!(
                "{:<62} {:>9} KB in  {:>7.1} MB peak  {:>7.1} MB past  {:>8.2?}  {outcome}",
                flood.name,
                bytes.len() >> 10,
                peak as f64 / 1e6,
                peak.saturating_sub(limits.max_read_bytes) as f64 / 1e6,
                took
            );
        }
        assert!(
            matches!(
                result,
                Ok(_) | Err(DocError::TooComplex(_) | DocError::Malformed(_))
            ),
            "{}: {outcome}",
            flood.name
        );
        // The budget is checked where the reader checks its deadline and as it makes its blocks:
        // a read passes it only by what it allocates between two checks, the text it reads then
        // twice over with the room a run's text doubles into (under two and a half times the
        // inflation cap), or a hash table's growth (less than that at these element caps).
        assert!(
            peak <= limits.max_read_bytes + 3 * limits.max_unzipped_bytes,
            "{}: {peak} bytes",
            flood.name
        );
        past = past.max(peak.saturating_sub(limits.max_read_bytes));
    }
    past
}

/// At a twentieth of the default caps every flood fits the budget a twentieth of the default
/// gives; under a budget of 8 MiB most pass it, and each stops at its next check.
#[test]
fn floods_are_refused_or_read_within_the_budget() {
    read_floods(&scaled(20), false);
    read_floods(
        &DocLimits {
            max_read_bytes: 8 << 20,
            ..scaled(20)
        },
        false,
    );
}

#[test]
#[ignore = "measures the floods at the default caps; run in release with --nocapture"]
fn floods_at_the_default_caps() {
    read_floods(&DocLimits::default(), true);
}

/// The floods at the default caps under budgets from 64 MiB to 1 GiB, which most pass: how far
/// past its budget a read goes between two checks.
#[test]
#[ignore = "measures how far one step goes past a budget; run in release with --nocapture"]
fn floods_past_smaller_budgets() {
    let mut past = 0;
    for mib in (64..=1_024).step_by(32) {
        let limits = DocLimits {
            max_read_bytes: mib << 20,
            ..DocLimits::default()
        };
        println!("max_read_bytes {mib} MiB");
        past = past.max(read_floods(&limits, true));
    }
    println!("most past a budget: {:.1} MB", past as f64 / 1e6);
}

/// Nested shadings of two candidate colours each would double the colours beneath a run at every
/// level: listed once each and capped, 40 levels read in little memory, the run undecided.
#[test]
fn shading_candidates_stay_few_however_deep() {
    let shaded = r#"<w:tbl><w:tr><w:tc><w:tcPr><w:shd w:val="pct50" w:color="123456" w:themeColor="accent1" w:fill="auto"/></w:tcPr>"#;
    let body = format!(
        "{}{}{}",
        shaded.repeat(40),
        para("", &run(r#"<w:color w:val="ABCDEF"/>"#, "x")),
        "</w:tc></w:tr></w:tbl>".repeat(40)
    );
    let limits = DocLimits {
        max_read_bytes: 64 << 20,
        ..DocLimits::default()
    };
    let (result, _, _) = counted(&Docx::new(&body).build(), &limits);
    let extracted = result.unwrap();
    assert!(extracted.blocks.is_empty());
    assert!(matches!(
        &extracted.warnings[..],
        [docs::ExtractWarning::HiddenContent {
            why: "undecided",
            ..
        }]
    ));
}

/// The two DOCX locators of one part share its name.
#[test]
fn a_parts_name_is_held_once() {
    let body = para("", &[run("", "a"), run("<w:vanish/>", "b")].concat()).repeat(3);
    let extracted = docs::extract(
        MediaKind::Docx,
        &Docx::new(&body).build(),
        &DocLimits::default(),
    )
    .unwrap();
    let mut names = extracted
        .blocks
        .iter()
        .map(|block| &block.locator)
        .chain(
            extracted
                .warnings
                .iter()
                .filter_map(|warning| match warning {
                    docs::ExtractWarning::HiddenContent { at, .. } => Some(at),
                    _ => None,
                }),
        )
        .map(|locator| match locator {
            Locator::DocxParagraph { part, .. } => part.clone(),
            _ => unreachable!("a DOCX locator"),
        });
    let first = names.next().unwrap();
    let mut count = 1;
    for name in names {
        assert!(Arc::ptr_eq(&first, &name));
        count += 1;
    }
    assert_eq!(count, 6);
}

/// Paragraphs that show nothing are not queued while their table is open: a cell of them costs
/// little whatever its length.
#[test]
fn quiet_table_paragraphs_are_not_queued() {
    let limits = DocLimits::default();
    for unit in [
        "<w:p></w:p>",
        r#"<w:p><w:r><w:t xml:space="preserve"> </w:t></w:r></w:p>"#,
        r#"<w:p><w:r><w:drawing/></w:r></w:p>"#,
    ] {
        let bytes = stored(&Docx::new(&format!(
            "<w:tbl><w:tr><w:tc>{}</w:tc></w:tr></w:tbl>",
            unit.repeat(200_000)
        )));
        let (result, peak, _) = counted(&bytes, &limits);
        assert!(result.unwrap().blocks.is_empty());
        // The package itself is most of it.
        assert!(peak < 3 * bytes.len() as u64 + (4 << 20), "{unit}: {peak}");
    }
}

/// A cell 80 tables deep costs what a cell of one table does: each nested container adds one
/// link to a chain its cells share, never a copy of the chain.
#[test]
fn nested_cells_cost_what_flat_cells_cost() {
    let limits = DocLimits::default();
    let cells = "<w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc>".repeat(40_000);
    let peak = |depth: usize| {
        let body = format!(
            "{}<w:tbl><w:tr>{cells}</w:tr></w:tbl>{}",
            "<w:tbl><w:tr><w:tc>".repeat(depth),
            "</w:tc></w:tr></w:tbl>".repeat(depth)
        );
        let (result, peak, _) = counted(&packed(&Docx::new(&body)), &limits);
        assert_eq!(result.unwrap().blocks.len(), 40_000);
        peak
    };
    let (flat, deep) = (peak(0), peak(79));
    assert!(deep < flat + flat / 4, "{flat} flat, {deep} deep");
}

/// A style that reads the same under every reading is held once: a style id defined twice and a
/// property written twice elsewhere cost what they cost, not four copies of every style.
#[test]
fn a_style_the_same_under_every_reading_is_held_once() {
    let limits = DocLimits::default();
    let regions: String = REGIONS
        .iter()
        .map(|kind| {
            format!(
                r#"<w:tblStylePr w:type="{kind}"><w:tcPr><w:shd w:val="clear" w:fill="FFFFFF"/></w:tcPr></w:tblStylePr>"#
            )
        })
        .collect();
    let styles: String = (0..4_000)
        .map(|at| format!(r#"<w:style w:type="table" w:styleId="t{at}">{regions}</w:style>"#))
        .collect();
    let tables: String = (0..4_000)
        .map(|at| {
            format!(
                r#"<w:tbl><w:tblPr><w:tblStyle w:val="t{at}"/></w:tblPr><w:tr><w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#
            )
        })
        .collect();
    let peak = |twice: &str, size: &str| {
        let docx = Docx::new(&format!("{}{tables}", para("", &run(size, "x"))))
            .styles(&format!("{twice}{styles}"));
        let (result, peak, _) = counted(&packed(&docx), &limits);
        assert_eq!(result.unwrap().blocks.len(), 4_001);
        peak
    };
    let once = peak("", "");
    let twice = peak(
        concat!(
            r#"<w:style w:type="paragraph" w:styleId="P"><w:rPr><w:sz w:val="20"/></w:rPr></w:style>"#,
            r#"<w:style w:type="paragraph" w:styleId="P"><w:rPr><w:sz w:val="22"/></w:rPr></w:style>"#
        ),
        r#"<w:sz w:val="20"/><w:sz w:val="22"/>"#,
    );
    assert!(
        twice < once + once / 4,
        "{once} once, {twice} with four readings"
    );
}
