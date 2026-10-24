//! `find_dates` and `compare_periods`.

use chrono::NaiveDate;
use katwork_thai::{
    Ctx, DateValue, Era, Flags, Found, NumberRole, PeriodCmp, PeriodKind, compare_periods,
    find_dates, find_numbers,
};

// Thai words, with every vowel and mark escaped so no editor can reorder them.
const PI: &str = "ป\u{0E35}"; // ปี
const FISCAL: &str = "ป\u{0E35}งบปร\u{0E30}ม\u{0E32}ณ"; // ปีงบประมาณ
const FISCAL_SHORT: &str = "ป\u{0E35}งบ"; // ปีงบ
const QUARTER: &str = "\u{0E44}ตรม\u{0E32}ส"; // ไตรมาส
const THI: &str = "ท\u{0E35}\u{0E48}"; // ที่
const WAN: &str = "ว\u{0E31}น"; // วัน
const TULAKHOM: &str = "ต\u{0E38}ล\u{0E32}คม"; // ตุลาคม
const TULA: &str = "ต\u{0E38}ล\u{0E32}"; // ตุลา
const CALENDAR: &str = "ป\u{0E35}ปฏ\u{0E34}ท\u{0E34}น"; // ปีปฏิทิน
const ACADEMIC: &str = "ป\u{0E35}ก\u{0E32}รศ\u{0E36}กษ\u{0E32}"; // ปีการศึกษา
const ACCOUNTS: &str = "ป\u{0E35}บ\u{0E31}ญช\u{0E35}"; // ปีบัญชี
const TAX: &str = "ป\u{0E35}ภ\u{0E32}ษ\u{0E35}"; // ปีภาษี
const BAHT: &str = "บ\u{0E32}ท"; // บาท
const KRANG: &str = "คร\u{0E31}\u{0E49}ง"; // ครั้ง
const MONTH_WORD: &str = "\u{0E40}ด\u{0E37}อน"; // เดือน

fn anchor() -> NaiveDate {
    NaiveDate::from_ymd_opt(2025, 11, 1).unwrap()
}

fn ctx() -> Ctx {
    Ctx {
        anchor: Some(anchor()),
    }
}

fn dates(text: &str) -> Vec<Found<DateValue>> {
    find_dates(text, &ctx()).unwrap()
}

fn only(text: &str) -> Found<DateValue> {
    let mut found = dates(text);
    assert_eq!(found.len(), 1, "{text:?}: {found:?}");
    found.remove(0)
}

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn fiscal(label: i32, start: NaiveDate, end: NaiveDate) -> DateValue {
    DateValue::Period {
        kind: PeriodKind::Fiscal,
        label,
        start,
        end,
    }
}

fn compare(a: &str, b: &str) -> PeriodCmp {
    compare_periods(&only(a).value, &only(b).value)
}

fn flag_names(flags: Flags) -> Vec<&'static str> {
    [
        (Flags::THAI_DIGITS, "THAI_DIGITS"),
        (Flags::MIXED_DIGITS, "MIXED_DIGITS"),
        (Flags::WORDS, "WORDS"),
        (Flags::TWO_DIGIT_YEAR, "TWO_DIGIT_YEAR"),
        (Flags::ERA_INFERRED, "ERA_INFERRED"),
        (Flags::FISCAL, "FISCAL"),
        (Flags::PRE_1941, "PRE_1941"),
        (Flags::OTHER_ERA, "OTHER_ERA"),
        (Flags::PAREN_NEGATIVE, "PAREN_NEGATIVE"),
        (Flags::TIME, "TIME"),
        (Flags::AMOUNT_IN_WORDS, "AMOUNT_IN_WORDS"),
    ]
    .into_iter()
    .filter(|(flag, _)| flags.contains(*flag))
    .map(|(_, name)| name)
    .collect()
}

fn parse_day(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
}

fn era(name: &str) -> Era {
    match name {
        "be" => Era::Be,
        "ce" => Era::Ce,
        "rattanakosin" => Era::Rattanakosin,
        "unknown" => Era::Unknown,
        other => panic!("unknown era {other}"),
    }
}

fn kind(name: &str) -> PeriodKind {
    match name {
        "calendar" => PeriodKind::Calendar,
        "fiscal" => PeriodKind::Fiscal,
        "academic" => PeriodKind::Academic,
        "unknown" => PeriodKind::Unknown,
        other => panic!("unknown period kind {other}"),
    }
}

fn expected_value(case: &serde_json::Value) -> DateValue {
    let int = |key: &str| case[key].as_i64().unwrap();
    if let Some(date) = case["day"].as_str() {
        DateValue::Day {
            date: parse_day(date),
            era: era(case["era"].as_str().unwrap()),
        }
    } else if let Some(month) = case["month"].as_str() {
        let (year, month) = month.split_once('-').unwrap();
        DateValue::Month {
            year: year.parse().unwrap(),
            month: month.parse().unwrap(),
            era: era(case["era"].as_str().unwrap()),
        }
    } else if case["year"].is_i64() {
        DateValue::Year {
            year: i32::try_from(int("year")).unwrap(),
            era: era(case["era"].as_str().unwrap()),
            period: kind(case["period"].as_str().unwrap()),
        }
    } else {
        DateValue::Period {
            kind: kind(case["period"].as_str().unwrap()),
            label: i32::try_from(int("label")).unwrap(),
            start: parse_day(case["start"].as_str().unwrap()),
            end: parse_day(case["end"].as_str().unwrap()),
        }
    }
}

#[test]
fn dates_match_fixture() {
    let path = format!("{}/tests/fixtures/dates.json", env!("CARGO_MANIFEST_DIR"));
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let fixture_anchor = parse_day(json["anchor"].as_str().unwrap());
    let cases = json["cases"].as_array().unwrap();
    assert!(
        cases.len() >= 60,
        "dates.json has only {} cases",
        cases.len()
    );
    let mut failures = Vec::new();
    for case in cases {
        let input = case["input"].as_str().unwrap();
        let found = find_dates(
            input,
            &Ctx {
                anchor: Some(fixture_anchor),
            },
        )
        .unwrap();
        let expected = case["expected"].as_array().unwrap();
        if found.len() != expected.len() {
            failures.push(format!(
                "{input:?}: found {found:?}, expected {} dates",
                expected.len()
            ));
            continue;
        }
        for (got, want) in found.iter().zip(expected) {
            let value = expected_value(want);
            let mut flags: Vec<&str> = want["flags"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| f.as_str().unwrap())
                .collect();
            flags.sort_unstable();
            let mut got_flags = flag_names(got.flags);
            got_flags.sort_unstable();
            let conf_ok = match &want["conf"] {
                serde_json::Value::String(band) if band == "high" => got.conf >= 0.8,
                serde_json::Value::String(band) if band == "low" => got.conf < 0.8,
                number => (f64::from(got.conf) - number.as_f64().unwrap()).abs() < 1e-6,
            };
            let raw_ok = want["raw"].as_str().is_none_or(|raw| raw == got.raw);
            if got.value != value || got_flags != flags || !conf_ok || !raw_ok {
                failures.push(format!(
                    "{input:?}: got {:?} {got_flags:?} conf {} raw {:?}; expected {value:?} {flags:?} conf {} raw {}",
                    got.value, got.conf, got.raw, want["conf"], want["raw"]
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} cases differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn be_and_ce_name_the_same_day() {
    let thai = only("1 ต.ค. 2568");
    let thai_digits = only(&format!(
        "\u{0E51} {TULAKHOM} \u{0E52}\u{0E55}\u{0E56}\u{0E58}"
    ));
    let english = only("1 October 2025");
    assert_eq!(
        thai.value,
        DateValue::Day {
            date: day(2025, 10, 1),
            era: Era::Be
        }
    );
    assert_eq!(
        thai_digits.value,
        DateValue::Day {
            date: day(2025, 10, 1),
            era: Era::Be
        }
    );
    assert_eq!(
        english.value,
        DateValue::Day {
            date: day(2025, 10, 1),
            era: Era::Ce
        }
    );
    assert!(thai_digits.flags.contains(Flags::THAI_DIGITS));
    assert_eq!(
        compare_periods(&thai.value, &english.value),
        PeriodCmp::Same
    );
}

#[test]
fn two_digit_year_is_low_confidence_with_alternative() {
    let found = only("1 ต.ค. 68");
    assert_eq!(
        found.value,
        DateValue::Day {
            date: day(2025, 10, 1),
            era: Era::Be
        }
    );
    assert!(found.conf < 0.8);
    assert!(found.flags.contains(Flags::TWO_DIGIT_YEAR));
    assert_eq!(found.alts.len(), 1);
    assert_eq!(
        found.alts[0].0,
        DateValue::Day {
            date: day(2068, 10, 1),
            era: Era::Ce
        }
    );
    assert!(found.alts[0].1 < found.conf);
}

#[test]
fn fiscal_year_is_october_to_september() {
    let year = only(&format!("{FISCAL} 2569"));
    assert_eq!(year.value, fiscal(2026, day(2025, 10, 1), day(2026, 9, 30)));
    assert!(year.flags.contains(Flags::FISCAL));
    assert_eq!(
        only(&format!("{QUARTER} 1 {FISCAL} 2569")).value,
        fiscal(2026, day(2025, 10, 1), day(2025, 12, 31))
    );
    assert_eq!(
        only("FY 2026").value,
        fiscal(2026, day(2025, 10, 1), day(2026, 9, 30))
    );
    assert_eq!(
        only("FY2569").value,
        fiscal(2026, day(2025, 10, 1), day(2026, 9, 30))
    );
}

#[test]
fn month_aliases_do_not_match_inside_numbers() {
    let found = only("10/11/68");
    assert_eq!(
        found.value,
        DateValue::Day {
            date: day(2025, 11, 10),
            era: Era::Be
        }
    );
    for text in ["1 ต.ค 2568", "1 ตค 2568", "1 ต. ค. 2568", "1 ต.ค. 2568"] {
        assert_eq!(
            only(text).value,
            DateValue::Day {
                date: day(2025, 10, 1),
                era: Era::Be
            },
            "{text:?}"
        );
    }
    for text in ["ต.ค 2568", "ตค 2568", "ต. ค. 2568"] {
        assert_eq!(
            only(text).value,
            DateValue::Month {
                year: 2025,
                month: 10,
                era: Era::Be
            },
            "{text:?}"
        );
    }
}

#[test]
fn bare_numbers_are_not_years() {
    for text in [
        "2,567".to_owned(),
        format!("2567 {BAHT}"),
        "ร\u{0E32}ค\u{0E32} 2568".to_owned(),
        "2567".to_owned(),
    ] {
        assert!(dates(&text).is_empty(), "{text:?}: {:?}", dates(&text));
    }
    assert!(dates(&format!("{PI} 2,567")).is_empty());
    assert!(dates(&format!("{PI} 2567.5")).is_empty());
}

#[test]
fn pre_1941_and_other_eras_are_flagged() {
    let old = only("พ.ศ. 2480");
    assert!(old.flags.contains(Flags::PRE_1941));
    assert!(old.conf < 0.8);
    assert!(only("พ.ศ. 2483").flags.contains(Flags::PRE_1941));
    assert!(!only("พ.ศ. 2484").flags.contains(Flags::PRE_1941));
    let rattanakosin = only("ร.ศ. 130");
    assert!(rattanakosin.flags.contains(Flags::OTHER_ERA));
    assert_eq!(
        rattanakosin.value,
        DateValue::Year {
            year: 1911,
            era: Era::Rattanakosin,
            period: PeriodKind::Unknown
        }
    );
    assert!(rattanakosin.conf < 0.8);
    let chula = only("จ.ศ. 1387");
    assert!(chula.flags.contains(Flags::OTHER_ERA));
    assert_eq!(
        chula.value,
        DateValue::Year {
            year: 1387,
            era: Era::Unknown,
            period: PeriodKind::Unknown
        }
    );
    assert!(chula.conf < 0.8);
    assert_eq!(
        compare_periods(&chula.value, &only(&format!("{PI} 2568")).value),
        PeriodCmp::Incomparable
    );
    assert_eq!(
        compare_periods(&only(&format!("{PI} 2568")).value, &chula.value),
        PeriodCmp::Incomparable
    );
}

#[test]
fn explicit_era_marker_accepts_out_of_range_years() {
    let ce = only("ค.ศ. 2567");
    assert_eq!(
        ce.value,
        DateValue::Year {
            year: 2567,
            era: Era::Ce,
            period: PeriodKind::Unknown
        }
    );
    assert!(!ce.flags.contains(Flags::ERA_INFERRED));
    assert_eq!(compare("ค.ศ. 2567", "พ.ศ. 2567"), PeriodCmp::Different);
    let be = only("พ.ศ. 2024");
    assert_eq!(
        be.value,
        DateValue::Year {
            year: 1481,
            era: Era::Be,
            period: PeriodKind::Unknown
        }
    );
    assert!(be.flags.contains(Flags::PRE_1941));
    assert_eq!(
        only("AD 1500").value,
        DateValue::Year {
            year: 1500,
            era: Era::Ce,
            period: PeriodKind::Unknown
        }
    );
    assert!(dates("ค.ศ. 999").is_empty());
    assert!(
        dates(&format!("{PI} 1500")).is_empty(),
        "without an era marker 1500 is not a year"
    );
}

#[test]
fn fiscal_quarters() {
    let quarters = [
        (day(2025, 10, 1), day(2025, 12, 31)),
        (day(2026, 1, 1), day(2026, 3, 31)),
        (day(2026, 4, 1), day(2026, 6, 30)),
        (day(2026, 7, 1), day(2026, 9, 30)),
    ];
    for (n, (start, end)) in (1..=4).zip(quarters) {
        for text in [
            format!("{QUARTER} {n} {FISCAL} 2569"),
            format!("{QUARTER}{THI} {n} {FISCAL} 2569"),
            format!("{QUARTER} {n} ของ{FISCAL} 2569"),
            format!("{FISCAL} 2569 {QUARTER} {n}"),
            format!("Q{n} FY2026"),
            format!("Q{n} FY 2026"),
        ] {
            let found = only(&text);
            assert_eq!(found.value, fiscal(2026, start, end), "{text:?}");
            assert!(found.flags.contains(Flags::FISCAL), "{text:?}");
        }
    }
    assert_eq!(
        compare("Q2 FY2026", &format!("{QUARTER}{THI} 2 {FISCAL} 2569")),
        PeriodCmp::Same
    );
    assert_eq!(compare("Q2 FY2026", "Q3 FY2026"), PeriodCmp::Different);
    assert!(dates(&format!("{QUARTER} 2")).is_empty());
    assert!(dates("Q2 revenue").is_empty());
    assert_eq!(
        only(&format!("{QUARTER} 5 {FISCAL} 2569")).value,
        fiscal(2026, day(2025, 10, 1), day(2026, 9, 30))
    );
}

#[test]
fn iso_and_hyphen_dates() {
    for text in ["2025-10-01", "2025/10/01", "1-10-2568"] {
        assert_eq!(
            only(text).value,
            DateValue::Day {
                date: day(2025, 10, 1),
                era: if text.ends_with("2568") {
                    Era::Be
                } else {
                    Era::Ce
                }
            },
            "{text:?}"
        );
    }
    assert!(only("2025-10-01").conf >= 0.8);
    assert!(only("2025/10/01").conf >= 0.8);
    assert!(only("1-10-2568").conf < 0.8);
    assert!(only("13/10/2568").conf >= 0.8);
    assert!(dates("2025-13-01").is_empty());
    assert!(dates("31/04/2568").is_empty());
}

#[test]
fn day_month_without_year_is_not_a_quantity() {
    let text = format!("ภ\u{0E32}ย\u{0E43}น{WAN}{THI} 15 ต.ค.");
    assert!(dates(&text).is_empty(), "{:?}", dates(&text));
    let numbers = find_numbers(&text, &ctx()).unwrap();
    assert_eq!(numbers.len(), 1, "{numbers:?}");
    assert_eq!(numbers[0].value.role, NumberRole::DatePart);
}

#[test]
fn year_marker_spellings() {
    for text in ["พ.ศ 2568", "พศ. 2568", "ค.ศ 2025", "คศ. 2025"] {
        let found = only(text);
        assert!(
            matches!(
                found.value,
                DateValue::Year {
                    year: 2025,
                    period: PeriodKind::Unknown,
                    ..
                }
            ),
            "{text:?}: {found:?}"
        );
    }
    for marker in [ACCOUNTS, TAX, &format!("รอบ{ACCOUNTS}")] {
        let text = format!("{marker} 2568");
        let found = only(&text);
        assert_eq!(
            found.value,
            DateValue::Year {
                year: 2025,
                era: Era::Be,
                period: PeriodKind::Unknown
            },
            "{text:?}"
        );
        assert_eq!(found.raw, text);
    }
}

#[test]
fn truncated_month_names_need_a_day_or_year() {
    assert_eq!(
        only(&format!("1 {TULA} 2568")).value,
        DateValue::Day {
            date: day(2025, 10, 1),
            era: Era::Be
        }
    );
    for text in ["may", "May", "กค", "ต.ค.", TULA] {
        assert!(dates(text).is_empty(), "{text:?}");
    }
    assert_eq!(
        only("May 2025").value,
        DateValue::Month {
            year: 2025,
            month: 5,
            era: Era::Ce
        }
    );
    assert!(dates("you may 25 times").is_empty());
}

#[test]
fn meeting_number_slash_year_is_not_a_date() {
    assert!(dates(&format!("{KRANG}{THI} 5/2568")).is_empty());
    assert!(dates("5/2568").is_empty());
}

#[test]
fn leap_day_uses_the_ce_year() {
    assert!(dates("29 ก.พ. 2568").is_empty());
    assert_eq!(
        only("29 ก.พ. 2567").value,
        DateValue::Day {
            date: day(2024, 2, 29),
            era: Era::Be
        }
    );
    assert!(dates("29/02/2568").is_empty());
}

#[test]
fn two_digit_year_tie_goes_to_be() {
    let be = day(2025, 10, 1);
    let ce = day(2068, 10, 1);
    let gap = (ce - be).num_days();
    assert_eq!(
        gap % 2,
        0,
        "pick a date whose readings are an even number of days apart"
    );
    let midpoint = be + chrono::Duration::days(gap / 2);
    let found = find_dates(
        "1 ต.ค. 68",
        &Ctx {
            anchor: Some(midpoint),
        },
    )
    .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].value,
        DateValue::Day {
            date: be,
            era: Era::Be
        }
    );
    assert_eq!(found[0].alts.len(), 1);
    assert_eq!(
        found[0].alts[0].0,
        DateValue::Day {
            date: ce,
            era: Era::Ce
        }
    );
    // One day later the CE reading is nearer.
    let later = find_dates(
        "1 ต.ค. 68",
        &Ctx {
            anchor: Some(midpoint + chrono::Duration::days(1)),
        },
    )
    .unwrap();
    assert_eq!(
        later[0].value,
        DateValue::Day {
            date: ce,
            era: Era::Ce
        }
    );
}

#[test]
fn academic_year_is_inconclusive() {
    let found = only(&format!("{ACADEMIC} 2568"));
    assert_eq!(
        found.value,
        DateValue::Year {
            year: 2025,
            era: Era::Be,
            period: PeriodKind::Academic
        }
    );
    assert!(found.conf < 0.8);
}

#[test]
fn marked_years_months_and_quarters_are_confident() {
    let year = only(&format!("{PI} 2567"));
    assert!(year.flags.contains(Flags::ERA_INFERRED));
    assert_eq!(year.conf, 0.9);
    for text in [
        "พ.ศ. 2567".to_owned(),
        format!("{CALENDAR} 2568"),
        format!("1 {TULAKHOM} 2568"),
    ] {
        assert_eq!(only(&text).conf, 0.9, "{text:?}");
    }
    assert_eq!(only(&format!("{QUARTER} 2 {FISCAL} 2569")).conf, 0.85);
    assert_eq!(only("Q2 FY2026").conf, 0.85);
    assert_eq!(only(&format!("{QUARTER} 2 {FISCAL_SHORT} 69")).conf, 0.6);
}

#[test]
fn period_comparison_table() {
    let fy = |y: &str| format!("{FISCAL} {y}");
    let year = |y: &str| format!("{PI} {y}");
    assert_eq!(compare(&year("2567"), &year("2024")), PeriodCmp::Same);
    assert_eq!(compare(&fy("2569"), "FY2026"), PeriodCmp::Same);
    assert_eq!(compare(&fy("2569"), "FY2568"), PeriodCmp::Different);
    assert_eq!(compare(&year("2567"), &year("2566")), PeriodCmp::Different);
    assert_eq!(compare(&fy("2568"), &year("2568")), PeriodCmp::Ambiguous);
    assert_eq!(
        compare(&fy("2568"), &format!("{CALENDAR} 2568")),
        PeriodCmp::KindDiffers
    );
    assert_eq!(
        compare("1 ต.ค. 2568", &year("2568")),
        PeriodCmp::Incomparable
    );
    // The rest of the table, by hand-built values.
    let y = |year, period| DateValue::Year {
        year,
        era: Era::Ce,
        period,
    };
    assert_eq!(
        compare_periods(
            &y(2025, PeriodKind::Calendar),
            &y(2025, PeriodKind::Academic)
        ),
        PeriodCmp::KindDiffers
    );
    assert_eq!(
        compare_periods(&y(2025, PeriodKind::Unknown), &y(2025, PeriodKind::Unknown)),
        PeriodCmp::Same
    );
    assert_eq!(
        compare_periods(
            &y(2025, PeriodKind::Academic),
            &y(2025, PeriodKind::Unknown)
        ),
        PeriodCmp::Ambiguous
    );
    let fy2026 = fiscal(2026, day(2025, 10, 1), day(2026, 9, 30));
    assert_eq!(
        compare_periods(&y(2026, PeriodKind::Calendar), &fy2026),
        PeriodCmp::KindDiffers
    );
    assert_eq!(
        compare_periods(&fy2026, &y(2026, PeriodKind::Calendar)),
        PeriodCmp::KindDiffers
    );
    assert_eq!(
        compare_periods(&fy2026, &y(1999, PeriodKind::Unknown)),
        PeriodCmp::Ambiguous
    );
    assert_eq!(
        compare_periods(&fy2026, &y(2026, PeriodKind::Academic)),
        PeriodCmp::Incomparable
    );
    let academic = DateValue::Period {
        kind: PeriodKind::Academic,
        label: 2026,
        start: day(2025, 10, 1),
        end: day(2026, 9, 30),
    };
    assert_eq!(compare_periods(&fy2026, &academic), PeriodCmp::KindDiffers);
    let month = |year, month| DateValue::Month {
        year,
        month,
        era: Era::Be,
    };
    assert_eq!(
        compare_periods(
            &month(2025, 10),
            &DateValue::Month {
                year: 2025,
                month: 10,
                era: Era::Ce
            }
        ),
        PeriodCmp::Same
    );
    assert_eq!(
        compare_periods(&month(2025, 10), &month(2025, 11)),
        PeriodCmp::Different
    );
    let unknown_month = DateValue::Month {
        year: 1387,
        month: 1,
        era: Era::Unknown,
    };
    assert_eq!(
        compare_periods(&unknown_month, &unknown_month),
        PeriodCmp::Incomparable
    );
    let d = |date| DateValue::Day { date, era: Era::Ce };
    assert_eq!(
        compare_periods(&d(day(2025, 10, 1)), &d(day(2025, 10, 2))),
        PeriodCmp::Different
    );
    assert_eq!(
        compare_periods(&d(day(2025, 10, 1)), &month(2025, 10)),
        PeriodCmp::Incomparable
    );
}

/// Coordinator ruling 5: Latin era markers count only in capitals.
#[test]
fn latin_era_markers_need_capitals() {
    for (text, year, era) in [
        ("BE 2567", 2024, Era::Be),
        ("2567 B.E.", 2024, Era::Be),
        ("2024 AD", 2024, Era::Ce),
        ("CE 2024", 2024, Era::Ce),
    ] {
        assert_eq!(
            only(text).value,
            DateValue::Year {
                year,
                era,
                period: PeriodKind::Unknown
            },
            "{text:?}"
        );
    }
    for text in ["to be 2025", "2025 ad", "the ce 2025"] {
        assert!(dates(text).is_empty(), "{text:?}: {:?}", dates(text));
    }
    assert_eq!(
        only("fy2026").value,
        fiscal(2026, day(2025, 10, 1), day(2026, 9, 30))
    );
    assert_eq!(
        only("CY2025").value,
        DateValue::Year {
            year: 2025,
            era: Era::Ce,
            period: PeriodKind::Calendar
        }
    );
    assert_eq!(
        only("calendar year 2025").value,
        DateValue::Year {
            year: 2025,
            era: Era::Ce,
            period: PeriodKind::Calendar
        }
    );
}

/// Coordinator ruling 4: a full Thai month name may follow Thai text, but nothing may continue it.
#[test]
fn month_names_glued_to_thai_text() {
    assert_eq!(
        only(&format!("{MONTH_WORD}{TULAKHOM} 2568")).value,
        DateValue::Month {
            year: 2025,
            month: 10,
            era: Era::Be
        }
    );
    assert!(dates(&format!("{TULA}ก\u{0E32}ร 2568")).is_empty());
    assert!(
        dates("กมค 2568").is_empty(),
        "a dotless abbreviation needs a non-Thai character before it"
    );
    assert_eq!(
        only("1 มค 2568").value,
        DateValue::Day {
            date: day(2025, 1, 1),
            era: Era::Be
        }
    );
    assert!(dates("1 Octobers 2025").is_empty());
}

/// Coordinator ruling 8: a two-digit year with an explicit era marker takes that era's century.
#[test]
fn two_digit_year_with_an_explicit_era() {
    let found = only("พ.ศ. 68");
    assert_eq!(
        found.value,
        DateValue::Year {
            year: 2025,
            era: Era::Be,
            period: PeriodKind::Unknown
        }
    );
    assert!(found.flags.contains(Flags::TWO_DIGIT_YEAR));
    assert!(!found.flags.contains(Flags::ERA_INFERRED));
    assert_eq!(found.conf, 0.6);
    assert!(found.alts.is_empty());
}

/// Coordinator ruling 8: English month-first dates and Sept.
#[test]
fn english_month_first_dates() {
    assert_eq!(
        only("October 1, 2025").value,
        DateValue::Day {
            date: day(2025, 10, 1),
            era: Era::Ce
        }
    );
    assert_eq!(
        only("Sept 5, 2025").value,
        DateValue::Day {
            date: day(2025, 9, 5),
            era: Era::Ce
        }
    );
    assert_eq!(
        only("Oct. 2025").value,
        DateValue::Month {
            year: 2025,
            month: 10,
            era: Era::Ce
        }
    );
}

#[test]
fn date_spans_cover_their_markers() {
    let text = format!("รวม{FISCAL} 2568 อย");
    let found = only(&text);
    assert_eq!(found.raw, format!("{FISCAL} 2568"));
    assert_eq!(found.start, 3);
    let year = format!("{PI} 2567");
    assert_eq!(only(&year).raw, year);
    let thai_digits =
        format!("{WAN}{THI} \u{0E51} ต.ค. \u{0E52}\u{0E55}\u{0E56}\u{0E58} ปร\u{0E30}ช\u{0E38}ม");
    assert_eq!(
        only(&thai_digits).raw,
        "\u{0E51} ต.ค. \u{0E52}\u{0E55}\u{0E56}\u{0E58}"
    );
}

#[test]
fn today_is_the_default_anchor() {
    let found = find_dates("1 ต.ค. 68", &Ctx { anchor: None }).unwrap();
    assert_eq!(found.len(), 1);
    assert!(found[0].flags.contains(Flags::TWO_DIGIT_YEAR));
}
