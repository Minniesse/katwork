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
            let alts: Vec<DateValue> = want["alts"]
                .as_array()
                .map(|alts| alts.iter().map(expected_value).collect())
                .unwrap_or_default();
            let alts_ok = want["alts"].is_null()
                || got.alts.iter().map(|(alt, _)| *alt).collect::<Vec<_>>() == alts;
            if got.value != value || got_flags != flags || !conf_ok || !raw_ok || !alts_ok {
                failures.push(format!(
                    "{input:?}: got {:?} {got_flags:?} conf {} raw {:?} alts {:?}; expected {value:?} {flags:?} conf {} raw {} alts {alts:?}",
                    got.value, got.conf, got.raw, got.alts, want["conf"], want["raw"]
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
    let today = Ctx {
        anchor: Some(chrono::Utc::now().date_naive()),
    };
    assert_eq!(found, find_dates("1 ต.ค. 68", &today).unwrap());
}

// Fix round 1: no endpoint of a range, and no year beside an unread quarter, reads at 0.8 or
// more; the digits inside them are date parts, never quantities.

const THUENG: &str = "ถ\u{0E36}ง"; // ถึง
const LAE: &str = "\u{0E41}ล\u{0E30}"; // และ
const TANGTAE: &str = "ต\u{0E31}\u{0E49}ง\u{0E41}ต\u{0E48}"; // ตั้งแต่
const THANWAKHOM: &str = "ธ\u{0E31}นว\u{0E32}คม"; // ธันวาคม
const YOTKHAI: &str = "ยอดข\u{0E32}ย"; // ยอดขาย
const RAYDAI: &str = "ร\u{0E32}ย\u{0E44}ด\u{0E49}"; // รายได้
const LAN: &str = "ล\u{0E49}\u{0E32}น"; // ล้าน

fn quantities(text: &str) -> Vec<String> {
    find_numbers(text, &ctx())
        .unwrap()
        .into_iter()
        .filter(|found| found.value.role == NumberRole::Quantity)
        .map(|found| found.raw)
        .collect()
}

/// Fix round 1: a range of years, months or days is one reading over the whole range at 0.5,
/// its start as the value and its end as the alternative.
#[test]
fn ranges_are_one_low_confidence_reading() {
    for (text, start, end) in [
        (
            format!("งบปร\u{0E30}ม\u{0E32}ณ{PI} 2567-2569"),
            year(2024),
            year(2026),
        ),
        ("พ.ศ. 2567 - 2569".to_owned(), year(2024), year(2026)),
        (format!("{PI} 2568 {THUENG} 2570"), year(2025), year(2027)),
        (format!("{PI} 2568 {LAE} 2569"), year(2025), year(2026)),
        (format!("{PI} 2567/68"), year(2024), year(2025)),
        (
            format!("{TANGTAE}{PI} 2567 {THUENG}{PI} 2569"),
            year(2024),
            year(2026),
        ),
        (
            "FY2024-2026".to_owned(),
            fiscal_year(2024),
            fiscal_year(2026),
        ),
        (
            "from FY2024 to FY2026".to_owned(),
            fiscal_year(2024),
            fiscal_year(2026),
        ),
        (
            "ต.ค.-ธ.ค. 2568".to_owned(),
            month(2025, 10),
            month(2025, 12),
        ),
        (
            format!("{TULAKHOM} - {THANWAKHOM} 2568"),
            month(2025, 10),
            month(2025, 12),
        ),
        (
            "Oct-Dec 2025".to_owned(),
            month_ce(2025, 10),
            month_ce(2025, 12),
        ),
        (
            "October to December 2025".to_owned(),
            month_ce(2025, 10),
            month_ce(2025, 12),
        ),
        (
            "1-31 ต.ค. 2568".to_owned(),
            on(2025, 10, 1),
            on(2025, 10, 31),
        ),
        (
            format!("15 - 17 {TULAKHOM} 2568"),
            on(2025, 10, 15),
            on(2025, 10, 17),
        ),
        (
            "1 ต.ค. 2568 - 31 ธ.ค. 2568".to_owned(),
            on(2025, 10, 1),
            on(2025, 12, 31),
        ),
        (
            format!("15 ต.ค. {THUENG} 31 ธ.ค. 2568"),
            on(2025, 10, 15),
            on(2025, 12, 31),
        ),
        (
            "Oct 1-15, 2025".to_owned(),
            on_ce(2025, 10, 1),
            on_ce(2025, 10, 15),
        ),
        (
            format!("{QUARTER} 1-2 {FISCAL} 2569"),
            fiscal(2026, day(2025, 10, 1), day(2025, 12, 31)),
            fiscal(2026, day(2026, 1, 1), day(2026, 3, 31)),
        ),
        (
            "Q1-Q2 FY2026".to_owned(),
            fiscal(2026, day(2025, 10, 1), day(2025, 12, 31)),
            fiscal(2026, day(2026, 1, 1), day(2026, 3, 31)),
        ),
    ] {
        let found = only(&text);
        assert!(found.conf < 0.8, "{text:?}: {found:?}");
        assert_eq!(found.value, start, "{text:?}");
        assert_eq!(found.alts.len(), 1, "{text:?}: {found:?}");
        assert_eq!(found.alts[0].0, end, "{text:?}");
        assert!(found.alts[0].1 < 0.8, "{text:?}");
        if text.starts_with(TANGTAE) {
            assert_eq!(found.start, 7, "{text:?}");
        }
        assert!(
            quantities(&text).is_empty(),
            "{text:?}: {:?}",
            quantities(&text)
        );
    }
    let budget = format!("งบปร\u{0E30}ม\u{0E32}ณ{PI} 2567-2569 รวม 1,250 {LAN}{BAHT}");
    assert_eq!(quantities(&budget), vec![format!("1,250 {LAN}")]);
    let sales = format!("{YOTKHAI} 1-31 ต.ค. 2568 รวม 1,250");
    assert_eq!(quantities(&sales), vec!["1,250".to_owned()]);
    // A figure after a marked year is no range, but a dash joining them is a signal of one: the
    // clean-context rule (fix round 2) keeps the year below 0.8. A minus is part of its figure.
    let figure = format!("{PI} 2567 - 5 {LAN}{BAHT}");
    assert_eq!(only(&figure).conf, 0.6);
    assert_eq!(quantities(&figure), vec![format!("5 {LAN}")]);
    let growth = format!("{PI} 2567 -25%");
    // No range: a year and one figure share a clean text (round 3b).
    assert_eq!(only(&growth).conf, 0.9);
    // ถึง also means "reached": a figure with an amount word after it ends no range, though the
    // ถึง still keeps the year below 0.8.
    let reached = format!("{YOTKHAI}{PI} 2567 {THUENG} 2570 {LAN}{BAHT}");
    assert!(only(&reached).value == year(2024), "{:?}", dates(&reached));
    assert_eq!(only(&reached).conf, 0.6);
    assert_eq!(quantities(&reached), vec![format!("2570 {LAN}")]);
    // Two days joined by และ stay two dates, each one item of a list, below 0.8; a bare day
    // before a day with no year is a part.
    let both = format!("1 ต.ค. 2568 {LAE} 2 ต.ค. 2568");
    assert_eq!(dates(&both).len(), 2, "{:?}", dates(&both));
    assert!(dates(&both).iter().all(|found| found.conf < 0.8));
    let days = "ปร\u{0E30}ช\u{0E38}ม 15-16 ต.ค.";
    assert!(quantities(days).is_empty(), "{:?}", quantities(days));
}

fn year(ce: i32) -> DateValue {
    DateValue::Year {
        year: ce,
        era: Era::Be,
        period: PeriodKind::Unknown,
    }
}

fn fiscal_year(label: i32) -> DateValue {
    fiscal(label, day(label - 1, 10, 1), day(label, 9, 30))
}

fn month(year: i32, month: u32) -> DateValue {
    DateValue::Month {
        year,
        month,
        era: Era::Be,
    }
}

fn month_ce(year: i32, month: u32) -> DateValue {
    DateValue::Month {
        year,
        month,
        era: Era::Ce,
    }
}

fn on(year: i32, month: u32, date: u32) -> DateValue {
    DateValue::Day {
        date: day(year, month, date),
        era: Era::Be,
    }
}

fn on_ce(year: i32, month: u32, date: u32) -> DateValue {
    DateValue::Day {
        date: day(year, month, date),
        era: Era::Ce,
    }
}

/// Fix round 1: a quarter beside a year that is not fiscal is not dropped: the year and the
/// quarter are one reading at 0.5, because which months the quarter covers is unknown.
#[test]
fn a_quarter_beside_a_year_is_low_confidence() {
    for text in [
        format!("{RAYDAI}{QUARTER} 2 {PI} 2568 1,250"),
        format!("{RAYDAI}{PI} 2568 {QUARTER} 2 1,250"),
        format!("{RAYDAI}{QUARTER}{THI} 2 {PI} 2568 1,250"),
        format!("{RAYDAI}{QUARTER} 2 ของ{PI} 2568 1,250"),
        format!("{RAYDAI}{QUARTER} 2 พ.ศ. 2568 1,250"),
        format!("{RAYDAI}{QUARTER} 2/2568 1,250"),
        "revenue Q2 CY2025 1,250".to_owned(),
        "revenue Q2 2025 1,250".to_owned(),
        "revenue CY2025 Q2 1,250".to_owned(),
    ] {
        let found = only(&text);
        assert!(found.conf < 0.8, "{text:?}: {found:?}");
        assert!(
            matches!(found.value, DateValue::Year { year: 2025, .. }),
            "{text:?}: {found:?}"
        );
        assert_eq!(quantities(&text), vec!["1,250".to_owned()], "{text:?}");
    }
}

/// Fix round 1: after a bare ปี, two digits that a figure precedes or that a duration, number,
/// scale or unit word follows are a length of time or an amount, not a year.
#[test]
fn two_digits_after_pi_before_an_amount_or_a_duration_are_no_year() {
    let income = format!("{RAYDAI}ต\u{0E48}อ{PI} 25 {LAN}{BAHT}");
    assert!(dates(&income).is_empty(), "{:?}", dates(&income));
    let amount = find_numbers(&income, &ctx()).unwrap();
    assert_eq!(amount.len(), 1);
    assert_eq!(amount[0].value.role, NumberRole::Quantity);
    assert_eq!(amount[0].value.effective().to_string(), "25000000");
    for text in [
        format!("อ\u{0E32}ย\u{0E38}ง\u{0E32}น 25 {PI} 10 {MONTH_WORD}"),
        format!("ครบ 3 {PI} 12 {MONTH_WORD}"),
    ] {
        assert!(dates(&text).is_empty(), "{text:?}: {:?}", dates(&text));
    }
    assert_eq!(only(&format!("{PI} 68")).conf, 0.6);
}

/// Fix round 1: a digit the fold does not cover, or a thin space, beside a day or a year keeps it
/// from being read alone.
#[test]
fn a_date_number_beside_an_unfolded_digit_or_a_thin_space_is_not_read_alone() {
    for text in ["\u{0ED1}5 ต.ค. 2568", "1\u{2009}5 ต.ค. 2568"] {
        assert!(
            dates(text)
                .iter()
                .all(|found| !matches!(found.value, DateValue::Day { .. })),
            "{text:?}: {:?}",
            dates(text)
        );
    }
    for text in [format!("{PI} 2568\u{0ED1}"), format!("{PI} 2568\u{2009}5")] {
        assert!(dates(&text).is_empty(), "{text:?}: {:?}", dates(&text));
    }
    assert_eq!(only("1\u{2063}5 ต.ค. 2568").value, on(2025, 10, 15));
}

/// Fix round 1 (mutants M4, D12, M8, M9, D06 and E04 to E08): what is not a date stays one.
#[test]
fn near_dates_are_not_dates() {
    for text in [
        format!("{PI} 2567,500"),
        format!("{PI} 12,500"),
        "1 ตคอ 2568".to_owned(),
        "xoct 2025".to_owned(),
        "dismay 2025".to_owned(),
        "1 ต.ค.ก 2568".to_owned(),
        "1/2/2568,3".to_owned(),
    ] {
        assert!(dates(&text).is_empty(), "{text:?}: {:?}", dates(&text));
    }
    // An abbreviation that begins a longer word is no month: สคบ. is an agency, not August.
    let agency = find_numbers("15 สคบ.", &ctx()).unwrap();
    assert_eq!(agency.len(), 1);
    assert_eq!(agency[0].value.role, NumberRole::Quantity);
    let judge = "15 ต\u{0E38}ล\u{0E32}ก\u{0E32}ร";
    assert!(dates(judge).is_empty());
    let number = find_numbers(judge, &ctx()).unwrap();
    assert_eq!(number.len(), 1);
    assert_eq!(number[0].value.role, NumberRole::Quantity);
    assert_eq!(only("ABQ2 FY2026").value, fiscal_year(2026));
    assert!(
        dates("3.1 ต.ค. 2568")
            .iter()
            .all(|found| !matches!(found.value, DateValue::Day { .. }))
    );
}

// Fix round 2: the clean-context rule (a founder decision). A date reads at 0.8 or more only when
// it matches a pattern the reader knows exactly and no signal of a range, a list, a fraction or
// (for a year, month or period) a part of it stands near it; otherwise at most 0.6. Ranges and
// lists the reader recognizes are one reading at 0.5 with the other items in `alts`.

const JONTHUENG: &str = "จนถ\u{0E36}ง"; // จนถึง
const PAI_JONTHUENG: &str = "\u{0E44}ปจนถ\u{0E36}ง"; // ไปจนถึง
const JONKRATHANG: &str = "จนกร\u{0E30}ท\u{0E31}\u{0E48}ง"; // จนกระทั่ง
const RAWANG: &str = "ร\u{0E30}หว\u{0E48}\u{0E32}ง"; // ระหว่าง
const JAK: &str = "จ\u{0E32}ก"; // จาก
const KAP: &str = "ก\u{0E31}บ"; // กับ
const NGUAT: &str = "งวด"; // งวด
const MONTHS_FIRST: &str = "\u{0E40}ด\u{0E37}อน\u{0E41}รก"; // เดือนแรก
const KHONG: &str = "ของ"; // ของ
const KHRUENG_PI: &str = "คร\u{0E36}\u{0E48}งป\u{0E35}\u{0E41}รก"; // ครึ่งปีแรก
const TON_PI: &str = "ต\u{0E49}นป\u{0E35}"; // ต้นปี
const SIN_PI: &str = "ส\u{0E34}\u{0E49}นป\u{0E35}"; // สิ้นปี
const SIP_HA: &str = "ส\u{0E34}บห\u{0E49}\u{0E32}"; // สิบห้า
const TRAIMAT_RAEK: &str = "\u{0E44}ตรม\u{0E32}ส\u{0E41}รก"; // ไตรมาสแรก
const ABROM: &str = "อบรมต\u{0E48}อ"; // อบรมต่อ
const CHUAMONG: &str = "ช\u{0E31}\u{0E48}ว\u{0E42}มง"; // ชั่วโมง
const SANYA: &str = "ส\u{0E31}ญญ\u{0E32}"; // สัญญา
const CHABAP: &str = "ฉบ\u{0E31}บ"; // ฉบับ
const ROI_LA: &str = "ร\u{0E49}อยล\u{0E30}"; // ร้อยละ
const HANG: &str = "ห\u{0E49}\u{0E32}งสรรพส\u{0E34}นค\u{0E49}\u{0E32}"; // ห้างสรรพสินค้า
const HAENG: &str = "\u{0E41}ห\u{0E48}ง"; // แห่ง
const SAMAT: &str = "ส\u{0E32}ม\u{0E32}รถ"; // สามารถ
const PHAYNAI: &str = "ภ\u{0E32}ย\u{0E43}น"; // ภายใน
const TO_PI: &str = "ต\u{0E48}อป\u{0E35}"; // ต่อปี
const SONG_RAI: &str = "ส\u{0E48}งร\u{0E32}ยง\u{0E32}น"; // ส่งรายงาน
const KHA_CHAI: &str = "ค\u{0E48}\u{0E32}\u{0E43}ช\u{0E49}จ\u{0E48}\u{0E32}ย"; // ค่าใช้จ่าย

/// One reading below 0.8 whose value is `start` and whose alternatives hold `others`, with no
/// digit of it read as a quantity.
fn one_low_reading(text: &str, start: DateValue, others: &[DateValue]) {
    let found = only(text);
    assert!(found.conf < 0.8, "{text:?}: {found:?}");
    assert_eq!(found.value, start, "{text:?}");
    let alts: Vec<DateValue> = found.alts.iter().map(|(alt, _)| *alt).collect();
    assert_eq!(alts, others, "{text:?}");
    assert!(
        quantities(text).is_empty(),
        "{text:?}: {:?}",
        quantities(text)
    );
}

/// Fix round 2: a range written with a filler word (ถึงวันที่, ถึงเดือน), a longer connector
/// (จนถึง, ไปจนถึง, จนกระทั่ง, `till`, `thru`, `up to and including`, an arrow) or a frame
/// (ระหว่าง … กับ, `between … and`) is one reading at 0.5, as the plain forms are.
#[test]
fn ranges_with_fillers_and_long_connectors_are_one_reading() {
    let october = on(2025, 10, 1);
    let december = on(2025, 12, 31);
    for text in [
        format!("{TANGTAE}{WAN}{THI} 1 ต.ค. 2568 {THUENG}{WAN}{THI} 31 ธ.ค. 2568"),
        format!("{RAWANG}{WAN}{THI} 1 ต.ค. 2568 {THUENG}{WAN}{THI} 31 ธ.ค. 2568"),
        format!("{JAK}{WAN}{THI} 1 ต.ค. 2568 {THUENG}{WAN}{THI} 31 ธ.ค. 2568"),
        format!("{TANGTAE} 1 ต.ค. 2568 {JONTHUENG} 31 ธ.ค. 2568"),
        format!("1 ต.ค. 2568 {PAI_JONTHUENG} 31 ธ.ค. 2568"),
        format!("1 ต.ค. 2568 {JONKRATHANG} 31 ธ.ค. 2568"),
        format!("{TANGTAE} 1 ต.ค. 2568 {THUENG}{WAN}{THI} 31 ธ.ค. 2568"),
        format!("{TANGTAE}{WAN}{THI} 1 ต.ค. 2568 - {WAN}{THI} 31 ธ.ค. 2568"),
        format!("{RAWANG} 1 ต.ค. 2568 {KAP} 31 ธ.ค. 2568"),
        format!("{WAN}{THI} 1 ต.ค. {THUENG}{WAN}{THI} 31 ธ.ค. 2568"),
    ] {
        one_low_reading(&text, october, &[december]);
    }
    let (october_ce, december_ce) = (on_ce(2025, 10, 1), on_ce(2025, 12, 31));
    for text in [
        "between 1 Oct 2025 and 31 Dec 2025",
        "from 1 October 2025 till 31 December 2025",
        "from 1 October 2025 thru 31 December 2025",
        "1 October 2025 -> 31 December 2025",
        "1 October 2025 \u{2192} 31 December 2025",
        "1 October 2025 up to and including 31 December 2025",
    ] {
        one_low_reading(text, october_ce, &[december_ce]);
    }
    one_low_reading(
        "between 1 and 15 October 2025",
        on_ce(2025, 10, 1),
        &[on_ce(2025, 10, 15)],
    );
    one_low_reading(
        &format!("{TANGTAE}{WAN}{THI} 1 {THUENG}{WAN}{THI} 15 {TULAKHOM} 2568"),
        on(2025, 10, 1),
        &[on(2025, 10, 15)],
    );
    one_low_reading(
        &format!("{TANGTAE}{MONTH_WORD}{TULAKHOM} 2568 {THUENG}{MONTH_WORD}{THANWAKHOM} 2568"),
        month(2025, 10),
        &[month(2025, 12)],
    );
    one_low_reading(
        &format!("{TANGTAE}{MONTH_WORD} ต.ค. {THUENG}{MONTH_WORD} ธ.ค. 2568"),
        month(2025, 10),
        &[month(2025, 12)],
    );
    one_low_reading(
        &format!("{PI} 2567 {JONTHUENG} 2569"),
        year(2024),
        &[year(2026)],
    );
    one_low_reading(
        &format!("{TANGTAE}{FISCAL} 2567 {JONTHUENG}{FISCAL} 2569"),
        fiscal_year(2024),
        &[fiscal_year(2026)],
    );
    // A right part with no year still keeps the left one below 0.8.
    let open = dates("15 ต.ค. 2568 - 15 พ.ย.");
    assert!(open.iter().all(|found| found.conf < 0.8), "{open:?}");
}

/// Fix round 2: numeric dates joined by a dash with no space, or an ISO interval, are one range
/// at 0.5; a date continued by any other figure still gives no date.
#[test]
fn numeric_dates_joined_with_no_space_are_one_range() {
    for text in [
        "1/10/2568-31/12/2568",
        "1.10.2568-31.12.2568",
        "01-10-2568-31-12-2568",
    ] {
        one_low_reading(text, on(2025, 10, 1), &[on(2025, 12, 31)]);
    }
    let short = only("01/10/68-31/12/68");
    assert_eq!(short.value, on(2025, 10, 1));
    assert!(short.conf < 0.8);
    one_low_reading(
        "2025-10-01/2025-12-31",
        on_ce(2025, 10, 1),
        &[on_ce(2025, 12, 31)],
    );
    for text in ["1/2/2568,3", "1/2/3/4", "1/10/2568-1234"] {
        assert!(
            dates(text).iter().all(|found| found.conf < 0.8),
            "{text:?}: {:?}",
            dates(text)
        );
    }
}

/// Fix round 2: years listed after one marker (commas, และ, `and`, spaces between) are one
/// reading at 0.5 with every other year in `alts`, and none of them is a quantity.
#[test]
fn lists_of_years_are_one_reading() {
    let list = format!("{RAYDAI}{PI} 2566, 2567 {LAE} 2568 รวม 3,750 {LAN}{BAHT}");
    let found = only(&list);
    assert_eq!(found.value, year(2023));
    assert_eq!(
        found.alts.iter().map(|(alt, _)| *alt).collect::<Vec<_>>(),
        [year(2024), year(2025)]
    );
    assert!(found.conf < 0.8);
    assert_eq!(quantities(&list), vec![format!("3,750 {LAN}")]);
    for text in [
        format!("{PI} 2566 2567 {LAE} 2568"),
        format!("{PI} 2566, 2567, 2568"),
        format!("พ.ศ. 2566, 2567 {LAE} 2568"),
        format!("{PI} 2566 {LAE} 2567 {LAE} 2568"),
    ] {
        one_low_reading(&text, year(2023), &[year(2024), year(2025)]);
    }
    for text in [
        format!("{FISCAL} 2566, 2567 {LAE} 2568"),
        "FY2023, FY2024 and FY2025".to_owned(),
        "FY2023, 2024 and 2025".to_owned(),
        "FY2023 & FY2024 or FY2025".to_owned(),
    ] {
        one_low_reading(
            &text,
            fiscal_year(2023),
            &[fiscal_year(2024), fiscal_year(2025)],
        );
    }
    // Two years with only a space between, and no connector after, stay apart.
    let pair = format!("{PI} 2566 2567");
    assert_eq!(only(&pair).raw, format!("{PI} 2566"));
}

/// Fix round 2: a word naming part of a year near a year, a month or a period (a quarter, a
/// half, N months, the start or end of the year) keeps it below 0.8; the plan's fiscal quarter,
/// with its own ไตรมาส, and a day stay confident.
#[test]
fn period_words_near_a_year_keep_it_below_0_8() {
    for text in [
        "FY2026 Q2 revenue 3,450".to_owned(),
        format!("{FISCAL} 2569 Q2 {RAYDAI} 3,450"),
        "Q2/FY2026 revenue 3,450".to_owned(),
        "FY2026 1Q revenue 3,450".to_owned(),
        "2Q FY2026".to_owned(),
        "Q2 of FY2026".to_owned(),
        "FY2026 H1".to_owned(),
        "FY2026 first quarter".to_owned(),
        "FY2026 second half".to_owned(),
        format!("{RAYDAI}{NGUAT} 9 {MONTHS_FIRST}{KHONG}{PI} 2568 1,250 {LAN}{BAHT}"),
        format!("{KHRUENG_PI}{KHONG}{PI} 2568 {RAYDAI} 1,250 {LAN}{BAHT}"),
        format!("{RAYDAI} 6 {MONTHS_FIRST}{PI} 2568"),
        format!("{TRAIMAT_RAEK}{KHONG}{PI} 2568"),
        format!("{TON_PI} 2568"),
        format!("{SIN_PI} 2568"),
        format!("{QUARTER} 1, 2 {LAE} 3 {FISCAL} 2569"),
    ] {
        let found = dates(&text);
        assert!(!found.is_empty(), "{text:?}");
        assert!(
            found.iter().all(|found| found.conf < 0.8),
            "{text:?}: {found:?}"
        );
    }
    // Fix rounds 3 and 4: every signal caps every reading of its text, a sign word a date too,
    // and a period word a day.
    for text in [
        format!("ผลปร\u{0E30}กอบก\u{0E32}รต\u{0E34}ดลบ\u{0E43}น{PI} 2568"),
        format!("{SONG_RAI}{KHA_CHAI}{QUARTER} 4 {PHAYNAI}{WAN}{THI} 31 ต.ค. 2568"),
    ] {
        assert_eq!(only(&text).conf, 0.6, "{text:?}");
    }
    for (text, conf) in [
        // A Latin signal inside a word is none (`to` in tonight, `sun` in sundry).
        (format!("tonight {PI} 2568"), 0.9),
        (format!("sundry {PI} 2568"), 0.9),
        // ต้น, กลาง, ปลาย and สิ้น are parts of a year only before ปี or เดือน: ต้นทุน is cost.
        (
            format!("\u{0E15}\u{0E49}\u{0E19}\u{0E17}\u{0E38}\u{0E19}{PI} 2568"),
            0.9,
        ),
        (format!("{QUARTER} 2 {FISCAL} 2569"), 0.85),
        ("Q2 FY2026".to_owned(), 0.85),
        (format!("{FISCAL} 2569"), 0.85),
        (format!("{PI} 2568"), 0.9),
    ] {
        assert_eq!(only(&text).conf, conf, "{text:?}");
    }
}

/// Fix round 2: a day the reader cannot read before a month name (a numeral it does not fold, a
/// separator, a lookalike letter, number words) leaves the month below 0.8 rather than read
/// alone; an English ordinal day and `25-Jan-2025` are days.
#[test]
fn a_day_the_reader_cannot_read_leaves_the_month_below_0_8() {
    for day in [
        "\u{0ED1}\u{0ED5}".to_owned(),
        "\u{0ED1}5".to_owned(),
        "1'5".to_owned(),
        "1\u{2009}5".to_owned(),
        "1\u{00B7}5".to_owned(),
        "15x".to_owned(),
        "1O".to_owned(),
        "l5".to_owned(),
        "I5".to_owned(),
        "1\u{FFFD}5".to_owned(),
        "\u{FFFD}5".to_owned(),
        "1\u{0096}5".to_owned(),
        SIP_HA.to_owned(),
    ] {
        let text = format!("{YOTKHAI} {day} ต.ค. 2568");
        let found = dates(&text);
        assert!(
            found.iter().all(|found| found.conf < 0.8),
            "{text:?}: {found:?}"
        );
        assert!(
            found
                .iter()
                .all(|found| !matches!(found.value, DateValue::Day { .. })),
            "{text:?}: {found:?}"
        );
    }
    for (text, date) in [
        ("1st October 2025", on_ce(2025, 10, 1)),
        ("the 3rd of October 2025", on_ce(2025, 10, 3)),
        ("October 1st, 2025", on_ce(2025, 10, 1)),
        ("31st December 2025", on_ce(2025, 12, 31)),
        ("22nd Dec 2025", on_ce(2025, 12, 22)),
        ("11th October 2025", on_ce(2025, 10, 11)),
        ("25-Jan-2025", on_ce(2025, 1, 25)),
        ("15Oct 2025", on_ce(2025, 10, 15)),
    ] {
        let found = only(text);
        assert_eq!(found.value, date, "{text:?}");
        assert_eq!(found.conf, 0.9, "{text:?}");
    }
    let wrong_suffix = dates("1th October 2025");
    assert!(wrong_suffix.iter().all(|found| found.conf < 0.8));
    let short = only("Jan-25");
    assert_eq!(short.value, month_ce(2025, 1));
    assert!(short.conf < 0.8);
    assert_eq!(only("Jan-2025").value, month_ce(2025, 1));
    // A dash right after a month name is no minus.
    for text in ["Jan-25", "Oct-5"] {
        assert!(
            find_numbers(text, &ctx())
                .unwrap()
                .iter()
                .all(|found| !found.value.value.is_sign_negative()),
            "{text:?}"
        );
    }
}

/// Fix round 2 (a regression of round 1): a control inside or before a date number keeps it
/// from being read; an information separator is a space, so the year before it is read.
#[test]
fn controls_keep_date_numbers_apart() {
    assert!(dates(&format!("{PI} 2567\u{0096}2569")).is_empty());
    let day = dates("ประชุม 1\u{0096}5 ต.ค. 2568");
    assert!(
        day.iter()
            .all(|found| !matches!(found.value, DateValue::Day { .. }) && found.conf < 0.8),
        "{day:?}"
    );
    let year = format!("{PI} 2567\u{001F}1,250 {LAN}{BAHT}");
    assert_eq!(only(&year).value, self::year(2024));
    // One text holds the year and one amount, which may share it (the founder's answer, round
    // 3b); a record separator splits nothing (round 4).
    assert_eq!(only(&year).conf, 0.9);
    let record = format!("{PI} 2567\u{001E}1,250 {LAN}{BAHT}");
    assert_eq!(only(&record).conf, 0.9);
    let budget = format!("{FISCAL} 2569\u{001F}1,250 {LAN}{BAHT}");
    assert_eq!(only(&budget).value, fiscal_year(2026));
    // A control right before a date number keeps it from being read too.
    assert!(dates(&format!("{PI} \u{0096}2568")).is_empty());
    let before_day = dates("\u{0096}15 ต.ค. 2568");
    assert!(
        before_day
            .iter()
            .all(|found| !matches!(found.value, DateValue::Day { .. }) && found.conf < 0.8),
        "{before_day:?}"
    );
}

/// Fix round 2 (round-1 review, P3): a bare four-digit year after a quarter with an amount word
/// after it is the amount; after a bare ปี, two digits before ร้อยละ, สามารถ or a word that begins
/// with a digit word are still a year.
#[test]
fn a_year_before_an_amount_word_is_the_amount_and_only_then() {
    let amount = format!("{QUARTER} 2 2568 {LAN}{BAHT}");
    assert!(dates(&amount).is_empty(), "{:?}", dates(&amount));
    assert_eq!(
        quantities(&amount),
        vec!["2".to_owned(), format!("2568 {LAN}")]
    );
    for text in [
        format!("{YOTKHAI}{PI} 68 {ROI_LA} 5"),
        format!("{PHAYNAI}{PI} 68 {SAMAT}"),
        format!("{PI} 68 {HANG} 3 {HAENG}"),
    ] {
        let found = only(&text);
        assert_eq!(found.value, year(2025), "{text:?}");
        assert!(found.flags.contains(Flags::TWO_DIGIT_YEAR), "{text:?}");
    }
    assert!(dates(&format!("{RAYDAI}{TO_PI} 25 {LAN}{BAHT}")).is_empty());
}

/// Fix round 2: the rule branches round 1 left untested (mutants A1, A2, B, G, X, Y, Z,
/// D-CONN-wordboundary, D-QUARTER-q5, D-RANGE-year-before, D-YEAR-delta, D-PIGUARD-figure,
/// D-PIGUARD-duration, D-TWODIGIT-ce and W-written).
#[test]
fn round_one_rule_branches_each_have_a_case() {
    for text in [
        format!("{PI} 2567\u{2014}2569"),
        format!("{PI} 2567 ~ 2569"),
        format!("{PI} 2567 {JONTHUENG} 2569"),
    ] {
        one_low_reading(&text, year(2024), &[year(2026)]);
    }
    for text in ["ต.ค.\u{2014}ธ.ค. 2568", "ต.ค. ~ ธ.ค. 2568"] {
        one_low_reading(text, month(2025, 10), &[month(2025, 12)]);
    }
    for text in ["FY2024 through FY2026", "FY2024 until FY2026"] {
        one_low_reading(text, fiscal_year(2024), &[fiscal_year(2026)]);
    }
    // `to` inside a word is no connector, so the bare year before it lists nothing.
    let tonight = dates("FY2024 tonight FY2026");
    assert_eq!(tonight.len(), 2, "{tonight:?}");
    assert!(tonight.iter().all(|found| found.conf == 0.6));
    let total = only("FY2026 2025 total");
    // FY2026 and the bare 2025, one date and one figure, share a clean text (round 3b).
    assert_eq!((total.conf, total.alts.len()), (0.85, 0), "{total:?}");
    // A range's left month after its end month is in the year before.
    one_low_reading("ต.ค.-ก.ย. 2568", month(2024, 10), &[month(2025, 9)]);
    // Two digits continue a range only 1 to 20 years on; four digits within 100 years.
    assert!(only(&format!("{PI} 2567-90")).alts.is_empty());
    assert_eq!(only(&format!("{PI} 2567-2667")).alts.len(), 1);
    assert!(only(&format!("{PI} 2567-2668")).alts.is_empty());
    // After a bare ปี, two digits are no year after a figure or before a duration.
    for text in [
        format!("{ABROM}{PI} 40 {CHUAMONG}"),
        format!("{SANYA} 3 {PI} 10 {CHABAP}"),
        format!("{PI} 10 {MONTH_WORD}"),
        format!("5 {PI} 10"),
    ] {
        assert!(dates(&text).is_empty(), "{text:?}: {:?}", dates(&text));
    }
    let slash = only(&format!("{QUARTER} 2/68"));
    assert_eq!(slash.value, year(2025));
    assert!(slash.conf < 0.8);
    assert!(dates("Q5 2025").is_empty());
    let century = only("ค.ศ. 25");
    assert_eq!(
        century.value,
        DateValue::Year {
            year: 2025,
            era: Era::Ce,
            period: PeriodKind::Unknown
        }
    );
    // A Latin era marker split by an invisible character is still read in capitals.
    assert_eq!(only("B\u{200E}E 2567").value, year(2024));
}

/// Fix round 2 (mutant D14): with no anchor, two-digit years are read nearest today, which
/// some year of every century tells apart from any other anchor more than a day away.
#[test]
fn no_anchor_reads_two_digit_years_nearest_today() {
    let today = Ctx {
        anchor: Some(chrono::Utc::now().date_naive()),
    };
    let year_ago = Ctx {
        anchor: today
            .anchor
            .and_then(|day| day.checked_sub_days(chrono::Days::new(365))),
    };
    let mut told_apart = false;
    for yy in 0..100 {
        let text = format!("1 ต.ค. {yy:02}");
        let found = find_dates(&text, &Ctx { anchor: None }).unwrap();
        assert_eq!(found, find_dates(&text, &today).unwrap(), "{text:?}");
        told_apart |= found != find_dates(&text, &year_ago).unwrap();
    }
    assert!(told_apart);
}
