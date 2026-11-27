use super::*;
use serde_json::json;

fn id() -> CitationId {
    CitationId::new()
}

fn claim(text: &str) -> Inline {
    Inline::Claim(ClaimInline {
        claim: ClaimSpec {
            text: text.into(),
            kind: ClaimKind::Number,
            cite: vec![id()],
            derive: None,
        },
    })
}

fn doc(blocks: Vec<ContentBlock>) -> WrittenContent {
    WrittenContent::Doc(DocContent {
        title: None,
        blocks,
    })
}

fn texts(content: &WrittenContent) -> Vec<(String, String, u32)> {
    uncited_claims(content)
        .unwrap()
        .into_iter()
        .map(|f| (f.claim.text, f.claim.location, f.after_claims))
        .collect()
}

#[test]
fn a1_names() {
    assert_eq!(a1(0, 0), "A1");
    assert_eq!(a1(1, 1), "B2");
    assert_eq!(a1(0, 25), "Z1");
    assert_eq!(a1(0, 26), "AA1");
    assert_eq!(a1(0, 701), "ZZ1");
    assert_eq!(a1(0, 702), "AAA1");
    assert_eq!(a1(1_048_575, 16_383), "XFD1048576");
}

#[test]
fn number_grammar() {
    for ok in ["0", "-0", "5", "1250", "-12.50", "0.001", "123456789"] {
        assert!(number_ok(ok), "{ok}");
    }
    for bad in [
        "", "1e5", "+5", ".5", "5.", "007", "1,234", "๑๒", "NaN", "inf", "-", "1.2.3", " 1",
    ] {
        assert!(!number_ok(bad), "{bad}");
    }
    assert!(!number_ok(&"9".repeat(400)));
    assert_eq!(significant_digits("1000"), 1);
    assert_eq!(significant_digits("0.00120"), 2);
    assert_eq!(significant_digits("1234567890123456"), 16);
}

#[test]
fn clean_text_makes_text_writable() {
    assert_eq!(
        clean_text("a\0b\u{1}c\u{B}d\u{FFFE}e\r\nf\tg\u{200B}ก"),
        "a\u{FFFD}b\u{FFFD}c\u{FFFD}d\u{FFFD}e f g\u{200B}ก"
    );
}

#[test]
fn uncited_figures_split_across_inlines_are_found() {
    let content = doc(vec![ContentBlock::Paragraph {
        text: vec![Inline::Text("งบ 3,45".into()), Inline::Text("0 บาท".into())],
    }]);
    let found = texts(&content);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].0.contains("3,450"), "{found:?}");
    assert_eq!(found[0].1, "block:1");
}

#[test]
fn ordinals_and_years_are_not_uncited_numbers() {
    let content = doc(vec![ContentBlock::Paragraph {
        text: vec![Inline::Text("วาระที่ 3".into())],
    }]);
    assert!(texts(&content).is_empty());
    let years = doc(vec![ContentBlock::Paragraph {
        text: vec![Inline::Text("ปี 2567".into())],
    }]);
    assert!(
        uncited_claims(&years)
            .unwrap()
            .iter()
            .all(|f| f.claim.kind == ClaimKind::Date)
    );
}

#[test]
fn ordinals_interleave_cited_and_uncited() {
    let content = doc(vec![ContentBlock::Paragraph {
        text: vec![
            claim("1"),
            Inline::Text(" งบประมาณ 3,450 บาท ".into()),
            claim("2"),
        ],
    }]);
    let uncited = uncited_claims(&content).unwrap();
    assert_eq!(uncited.len(), 1);
    assert_eq!(uncited[0].after_claims, 1);
    assert_eq!(cited_ordinals(2, &uncited), vec![1, 3]);
    assert_eq!(number_uncited(uncited)[0].ordinal, 2);
    // Two uncited figures before any claim.
    let content = doc(vec![
        ContentBlock::Paragraph {
            text: vec![Inline::Text("ยอด 1,250 บาท และ 3,400 บาท".into())],
        },
        ContentBlock::Paragraph {
            text: vec![claim("x")],
        },
    ]);
    let uncited = uncited_claims(&content).unwrap();
    assert_eq!(uncited.len(), 2);
    assert_eq!(cited_ordinals(1, &uncited), vec![3]);
    let numbered = number_uncited(uncited);
    assert_eq!((numbered[0].ordinal, numbered[1].ordinal), (1, 2));
}

#[test]
fn figures_in_title_lists_tables_cells_and_sheet_names() {
    let content = WrittenContent::Doc(DocContent {
        title: Some("รายงานงบ 9,999 บาท".into()),
        blocks: vec![
            ContentBlock::List {
                ordered: true,
                items: vec![vec![Inline::Text("ค่าใช้จ่าย 1,200 บาท".into())]],
            },
            ContentBlock::Table {
                header: vec![vec![Inline::Text("หัว".into())]],
                rows: vec![vec![vec![Inline::Text("รวม 4,500 บาท".into())]]],
            },
        ],
    });
    let found: Vec<String> = texts(&content).into_iter().map(|t| t.1).collect();
    assert_eq!(found, vec!["block:0", "block:1", "block:2"]);
    let sheets = WrittenContent::Sheets(vec![SheetSpec {
        name: "งบ 9,999 บาท".into(),
        rows: vec![
            vec![CellSpec::Empty],
            vec![
                CellSpec::Empty,
                CellSpec::Number(NumberCell {
                    number: "9999".into(),
                }),
            ],
            vec![CellSpec::Empty, CellSpec::Text("งบ 3,450 บาท".into())],
        ],
    }]);
    let found: Vec<String> = texts(&sheets).into_iter().map(|t| t.1).collect();
    assert_eq!(
        found,
        vec![
            "sheet:งบ 9,999 บาท",
            "cell:งบ 9,999 บาท!B2",
            "cell:งบ 9,999 บาท!B3"
        ]
    );
}

#[test]
fn uncited_window_cuts_at_graphemes() {
    // A combining mark sits exactly where a code-point cut would split its cluster.
    let left = format!("{}{}", "ก".repeat(2), "กี".repeat(16));
    let run = format!("{left} 1,250 บาท");
    let found = texts(&doc(vec![ContentBlock::Paragraph {
        text: vec![Inline::Text(run.clone())],
    }]));
    assert_eq!(found.len(), 1);
    let text = &found[0].0;
    assert!(text.contains("1,250"));
    assert!(!text.starts_with('\u{0E35}'), "{text}");
    assert!(run.contains(text.as_str()));
    assert!(text.chars().count() <= 5 + 2 * UNCITED_CONTEXT);
}

#[test]
fn grammar_refuses_locators_and_unknown_fields() {
    let cite = id();
    for bad in [
        json!({"blocks":[{"type":"paragraph","text":[{"claim":{"text":"1","kind":"number","cite":[cite],"page":3}}]}]}),
        json!({"blocks":[{"type":"paragraph","text":[{"claim":{"text":"1","kind":"number","cite":[cite]},"page":3}]}]}),
        json!({"blocks":[{"type":"paragraph","text":[],"page":1}]}),
        json!({"blocks":[{"type":"image","src":"x"}]}),
        json!({"blocks":[],"file":"x"}),
        json!({"blocks":[{"type":"paragraph","text":[{"link":"http://x"}]}]}),
    ] {
        assert!(
            serde_json::from_value::<DocContent>(bad.clone()).is_err(),
            "{bad}"
        );
    }
    for bad in [
        json!({"name":"S","rows":[[{"number":"1","cell":"A1"}]]}),
        json!({"name":"S","rows":[[{"formula":"=1"}]]}),
        json!({"name":"S","rows":[[1]]}),
    ] {
        assert!(
            serde_json::from_value::<SheetSpec>(bad.clone()).is_err(),
            "{bad}"
        );
    }
    let ok: SheetSpec =
        serde_json::from_value(json!({"name":"S","rows":[[null,"x",{"number":"1"}]]})).unwrap();
    assert_eq!(ok.rows[0][0], CellSpec::Empty);
}

#[test]
fn content_round_trips_through_serde() {
    let cite = id();
    let value = json!({"title":"T","blocks":[
        {"type":"heading","level":1,"text":["H"]},
        {"type":"paragraph","text":["a",{"claim":{"text":"1","kind":"number","cite":[cite],"derive":{"op":"sum","args":[0,0]}}}]},
        {"type":"list","ordered":false,"items":[["x"]]},
        {"type":"table","header":[["h"]],"rows":[[["c"]]]}]});
    let content: DocContent = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(&content).unwrap(), value);
}

#[test]
fn claim_bounds() {
    let check = |claim: ClaimSpec| {
        let mut content = DocContent {
            title: None,
            blocks: vec![ContentBlock::Paragraph {
                text: vec![Inline::Claim(ClaimInline { claim })],
            }],
        };
        check_doc(&mut content)
    };
    let base = ClaimSpec {
        text: "1".into(),
        kind: ClaimKind::Number,
        cite: vec![id()],
        derive: None,
    };
    assert!(check(base.clone()).is_ok());
    let one = id();
    for bad in [
        ClaimSpec {
            text: String::new(),
            ..base.clone()
        },
        ClaimSpec {
            text: "ก".repeat(501),
            ..base.clone()
        },
        ClaimSpec {
            text: " \u{200B} ".into(),
            ..base.clone()
        },
        ClaimSpec {
            cite: vec![],
            ..base.clone()
        },
        ClaimSpec {
            cite: (0..9).map(|_| id()).collect(),
            ..base.clone()
        },
        ClaimSpec {
            cite: vec![one, one],
            ..base.clone()
        },
        ClaimSpec {
            kind: ClaimKind::Quote,
            derive: Some(serde_json::from_value(json!({"op":"sum","args":[0,0]})).unwrap()),
            ..base.clone()
        },
        ClaimSpec {
            cite: vec![id(), id()],
            derive: Some(serde_json::from_value(json!({"op":"sum","args":[0,5]})).unwrap()),
            ..base.clone()
        },
    ] {
        assert!(check(bad.clone()).is_err(), "{bad:?}");
    }
    assert!(
        check(ClaimSpec {
            text: "ก".repeat(500),
            cite: (0..8).map(|_| id()).collect(),
            ..base
        })
        .is_ok()
    );
}

#[test]
fn content_caps() {
    let para = |text: &str| ContentBlock::Paragraph {
        text: vec![Inline::Text(text.into())],
    };
    let mut many = DocContent {
        title: None,
        blocks: (0..=MAX_BLOCKS).map(|_| para("")).collect(),
    };
    assert_eq!(
        check_doc(&mut many),
        Err(ContentError("content_too_many_blocks"))
    );
    let mut long = DocContent {
        title: Some("ก".repeat(MAX_CHARS)),
        blocks: vec![para("x")],
    };
    assert_eq!(check_doc(&mut long), Err(ContentError("content_too_long")));
    let mut cells = DocContent {
        title: None,
        blocks: vec![ContentBlock::Table {
            header: vec![],
            rows: (0..400).map(|_| vec![vec![]; 60]).collect(),
        }],
    };
    assert_eq!(
        check_doc(&mut cells),
        Err(ContentError("content_too_many_pieces"))
    );
    // One wide header row and many empty rows: the renderer pads each row to 63 cells.
    let mut padded = DocContent {
        title: None,
        blocks: vec![ContentBlock::Table {
            header: vec![vec![]; MAX_TABLE_COLUMNS],
            rows: vec![vec![]; 20_000],
        }],
    };
    assert_eq!(
        check_doc(&mut padded),
        Err(ContentError("content_too_many_pieces"))
    );
    let mut items = DocContent {
        title: None,
        blocks: vec![ContentBlock::List {
            ordered: false,
            items: vec![vec![]; MAX_PIECES + 1],
        }],
    };
    assert_eq!(
        check_doc(&mut items),
        Err(ContentError("content_too_many_pieces"))
    );
    let mut levels = DocContent {
        title: None,
        blocks: vec![ContentBlock::Heading {
            level: 4,
            text: vec![],
        }],
    };
    assert_eq!(check_doc(&mut levels), Err(ContentError("heading_level")));
    levels.blocks = vec![ContentBlock::Heading {
        level: 0,
        text: vec![],
    }];
    assert!(check_doc(&mut levels).is_err());
}

#[test]
fn xlsx_caps() {
    let sheet = |name: &str| SheetSpec {
        name: name.into(),
        rows: vec![],
    };
    let check = |mut sheets: Vec<SheetSpec>| check_sheets(&mut sheets);
    assert!(check(vec![sheet("Sheet1")]).is_ok());
    assert!(check(vec![]).is_err());
    assert!(check((0..17).map(|i| sheet(&format!("S{i}"))).collect()).is_err());
    for bad in [
        "a".repeat(32).as_str(),
        "",
        "a:b",
        "'a",
        "a'",
        "a[1]",
        "x/y",
        "History",
        "history",
        "a\u{1}",
    ] {
        assert!(check(vec![sheet(bad)]).is_err(), "{bad}");
    }
    assert!(check(vec![sheet(&"ก".repeat(31))]).is_ok());
    assert!(check(vec![sheet("A!B")]).is_ok());
    assert!(check(vec![sheet("Data"), sheet("DATA")]).is_err());
    assert!(check(vec![sheet("ข้อมูล"), sheet("ข้อมูล")]).is_err());
    let cells = |rows: Vec<Vec<CellSpec>>| {
        check(vec![SheetSpec {
            name: "S".into(),
            rows,
        }])
    };
    assert!(cells(vec![vec![CellSpec::Empty; 10_000]; 5]).is_ok());
    assert!(
        cells(vec![
            vec![CellSpec::Empty; 10_001],
            vec![CellSpec::Empty; 40_000]
        ])
        .is_err()
    );
    assert_eq!(
        cells(vec![vec![CellSpec::Empty; MAX_COLUMNS + 1]]),
        Err(ContentError("sheet_too_many_columns"))
    );
    assert!(cells(vec![vec![CellSpec::Text("ก".repeat(MAX_CELL_CHARS + 1))]]).is_err());
    assert!(cells(vec![vec![CellSpec::Text("ก".repeat(MAX_CELL_CHARS))]]).is_ok());
    assert!(
        cells(vec![vec![CellSpec::Number(NumberCell {
            number: "1e5".into()
        })]])
        .is_err()
    );
    let mut rows = SheetSpec {
        name: "S".into(),
        rows: vec![],
    };
    rows.rows = (0..=MAX_ROWS).map(|_| vec![]).collect();
    assert_eq!(
        check_sheets(std::slice::from_mut(&mut rows)),
        Err(ContentError("sheet_too_many_rows"))
    );
    // Over the character cap across cells.
    assert!(
        cells(
            (0..7)
                .map(|_| vec![CellSpec::Text("ก".repeat(30_000))])
                .collect()
        )
        .is_err()
    );
}
