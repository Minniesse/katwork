//! `parse_number_words`, `find_numbers` and `fold_digit`.

use chrono::NaiveDate;
use katwork_thai::{
    Ctx, Flags, Form, Found, MAX_SCAN_CODE_POINTS, Number, NumberRole, Scale, TooLong, Unit,
    find_dates, find_numbers, fold_digit, parse_number_words,
};
use rust_decimal::Decimal;

// Thai words, with every vowel and mark escaped so no editor can reorder them.
const NUENG: &str = "หน\u{0E36}\u{0E48}ง"; // หนึ่ง
const SONG: &str = "สอง";
const SAM: &str = "ส\u{0E32}ม"; // สาม
const HA: &str = "ห\u{0E49}\u{0E32}"; // ห้า
const HOK: &str = "หก";
const KAO: &str = "\u{0E40}ก\u{0E49}\u{0E32}"; // เก้า
const SIP: &str = "ส\u{0E34}บ"; // สิบ
const ROI: &str = "ร\u{0E49}อย"; // ร้อย
const PHAN: &str = "พ\u{0E31}น"; // พัน
const MUEN: &str = "หม\u{0E37}\u{0E48}น"; // หมื่น
const SAEN: &str = "\u{0E41}สน"; // แสน
const LAN: &str = "ล\u{0E49}\u{0E32}น"; // ล้าน
const LOP: &str = "ลบ";
const CHUT: &str = "จ\u{0E38}ด"; // จุด
const BAHT: &str = "บ\u{0E32}ท"; // บาท
const THUAN: &str = "ถ\u{0E49}วน"; // ถ้วน
const ROI_LA: &str = "ร\u{0E49}อยล\u{0E30}"; // ร้อยละ
const PERCENT_TH: &str = "\u{0E40}ปอร\u{0E4C}\u{0E40}ซ\u{0E47}นต\u{0E4C}"; // เปอร์เซ็นต์
const PERCENT_TH_SHORT: &str = "\u{0E40}ปอร\u{0E4C}\u{0E40}ซนต\u{0E4C}"; // เปอร์เซนต์
const RAYDAI: &str = "ร\u{0E32}ย\u{0E44}ด\u{0E49}"; // รายได้
const PI: &str = "ป\u{0E35}"; // ปี
const FISCAL: &str = "ป\u{0E35}งบปร\u{0E30}ม\u{0E32}ณ"; // ปีงบประมาณ
const THI: &str = "ท\u{0E35}\u{0E48}"; // ที่
const WAN: &str = "ว\u{0E31}น"; // วัน
const KHON: &str = "คน";

fn ctx() -> Ctx {
    Ctx {
        anchor: NaiveDate::from_ymd_opt(2025, 11, 1),
    }
}

fn numbers(text: &str) -> Vec<Found<Number>> {
    find_numbers(text, &ctx()).unwrap()
}

fn only(text: &str) -> Found<Number> {
    let mut found = numbers(text);
    assert_eq!(found.len(), 1, "{text:?}: {found:?}");
    found.remove(0)
}

fn quantities(text: &str) -> Vec<Found<Number>> {
    numbers(text)
        .into_iter()
        .filter(|found| found.value.role == NumberRole::Quantity)
        .collect()
}

fn dec(text: &str) -> Decimal {
    Decimal::from_str_exact(text).unwrap()
}

fn span(text: &str, found: &Found<Number>) -> String {
    let start = usize::try_from(found.start).unwrap();
    let end = usize::try_from(found.end).unwrap();
    text.chars().skip(start).take(end - start).collect()
}

fn values(text: &str) -> Vec<Decimal> {
    numbers(text)
        .iter()
        .map(|found| found.value.value)
        .collect()
}

fn thb() -> Option<Unit> {
    Some(Unit::Currency("THB"))
}

#[test]
fn number_words_match_oracle() {
    let path = format!("{}/tests/fixtures/numbers.json", env!("CARGO_MANIFEST_DIR"));
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let cases: Vec<(String, Option<String>)> = json["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| {
            (
                case["input"].as_str().unwrap().to_owned(),
                case["expected"].as_str().map(str::to_owned),
            )
        })
        .collect();
    assert!(
        cases.len() >= 200,
        "numbers.json has only {} cases",
        cases.len()
    );
    let failures: Vec<_> = cases
        .iter()
        .filter_map(|(input, expected)| {
            let got = parse_number_words(input).map(|value| value.to_string());
            (got != *expected).then(|| format!("{input:?}: got {got:?}, oracle {expected:?}"))
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} cases differ:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
    for (input, value) in [
        (
            format!("{SONG}{LAN}{SAM}{SAEN}{HOK}{ROI}{SIP}{SONG}"),
            "2300612",
        ),
        (format!("{LOP}{NUENG}{ROI}"), "-100"),
        (LAN.to_owned(), "1000000"),
        (format!("{SIP}{LAN}"), "10000000"),
        (format!("{NUENG}{PHAN}{LAN}"), "1000000000"),
        (format!("{HA}{SIP}{CHUT}{KAO}{HA}"), "50.95"),
    ] {
        assert_eq!(parse_number_words(&input), Some(dec(value)), "{input:?}");
        assert!(
            cases.contains(&(input.clone(), Some(value.to_owned()))),
            "{input:?} is not in numbers.json"
        );
    }
}

#[test]
fn parse_number_words_returns_none_on_overflow() {
    assert_eq!(
        parse_number_words(&LAN.repeat(4)),
        Some(dec("1000000000000000000000000"))
    );
    assert_eq!(parse_number_words(&LAN.repeat(5)), None);
    assert_eq!(parse_number_words(&LAN.repeat(60)), None);
    let decimals = format!("{NUENG}{CHUT}{}", NUENG.repeat(29));
    assert_eq!(parse_number_words(&decimals), None);
}

#[test]
fn scales_compose_to_powers_of_ten() {
    let trillions = only(&format!("3.48 {LAN}{LAN}{BAHT}"));
    assert_eq!(trillions.value.scale, Scale(12));
    assert_eq!(trillions.value.effective(), dec("3480000000000"));
    assert_eq!(only(&format!("2 {MUEN}{LAN}")).value.scale, Scale(10));
    assert_eq!(only(&format!("1.25 {PHAN}{LAN}")).value.scale, Scale(9));
    assert_eq!(only(&format!("4 {SAEN}{LAN}")).value.scale, Scale(11));
    assert_eq!(only(&format!("4 {PHAN}")).value.scale, Scale(3));
    assert_eq!(only(&format!("4 {MUEN}")).value.scale, Scale(4));
    assert_eq!(only(&format!("4 {SAEN}")).value.scale, Scale(5));
    assert_eq!(only(&format!("4 {LAN}")).value.scale, Scale(6));
    assert_eq!(Scale(12).multiplier(), dec("1000000000000"));
    assert_eq!(Scale(0).multiplier(), Decimal::ONE);
}

#[test]
fn finds_figures_with_scale_unit_and_digits() {
    let found = numbers(&format!("{RAYDAI}รวม ๑,๒๕๐ {LAN}{BAHT}"));
    assert_eq!(found.len(), 1, "{found:?}");
    let number = &found[0].value;
    assert_eq!(number.role, NumberRole::Quantity);
    assert_eq!(number.value, dec("1250"));
    assert_eq!(number.scale, Scale(6));
    assert_eq!(number.unit, thb());
    assert_eq!(number.form, Form::Digits);
    assert!(found[0].flags.contains(Flags::THAI_DIGITS));
    assert!(!found[0].flags.contains(Flags::MIXED_DIGITS));
    assert_eq!(number.effective(), dec("1250000000"));
}

#[test]
fn accounting_negative_and_minus() {
    let paren = only("(1,250)");
    assert_eq!(paren.value.value, dec("-1250"));
    assert!(paren.flags.contains(Flags::PAREN_NEGATIVE));
    let minus = only("\u{2212}1,250");
    assert_eq!(minus.value.value, dec("-1250"));
    assert!(!minus.flags.contains(Flags::PAREN_NEGATIVE));
    assert_eq!(only("-1,250").value.value, dec("-1250"));
    assert_eq!(only("\u{2012}7").value.value, dec("-7"));
    assert_eq!(only("\u{2013}7").value.value, dec("-7"));
}

#[test]
fn percent_forms() {
    for text in [
        "12.5%".to_owned(),
        format!("{ROI_LA} 12.5"),
        format!("12.5 {PERCENT_TH}"),
    ] {
        let found = only(&text);
        assert_eq!(found.value.value, dec("12.5"), "{text:?}");
        assert_eq!(found.value.unit, Some(Unit::Percent), "{text:?}");
        assert_eq!(found.value.decimals, 1, "{text:?}");
        assert_eq!(found.value.role, NumberRole::Quantity, "{text:?}");
    }
}

#[test]
fn half_ulp_follows_displayed_precision() {
    assert_eq!(
        only(&format!("1.25 {PHAN}{LAN}")).value.half_ulp(),
        dec("5000000")
    );
    assert_eq!(only("1,250").value.half_ulp(), dec("0.5"));
    assert_eq!(only("3.100").value.half_ulp(), dec("0.0005"));
    assert_eq!(only("3.100").value.decimals, 3);
    assert_eq!(
        only(&format!("2 {MUEN}{LAN}")).value.half_ulp(),
        dec("5000000000")
    );
}

#[test]
fn years_in_periods_are_date_parts() {
    let text = format!("{RAYDAI}{PI} 2567 1,250 {LAN}{BAHT}");
    let found = numbers(&text);
    let quantities: Vec<_> = found
        .iter()
        .filter(|f| f.value.role == NumberRole::Quantity)
        .collect();
    assert_eq!(quantities.len(), 1, "{found:?}");
    assert_eq!(quantities[0].value.value, dec("1250"));
    let year = found.iter().find(|f| f.value.value == dec("2567")).unwrap();
    assert_eq!(year.value.role, NumberRole::DatePart);
    let fiscal = numbers("FY2569 budget 3,450");
    assert_eq!(
        fiscal
            .iter()
            .find(|f| f.value.value == dec("2569"))
            .unwrap()
            .value
            .role,
        NumberRole::DatePart
    );
    assert_eq!(
        fiscal
            .iter()
            .find(|f| f.value.value == dec("3450"))
            .unwrap()
            .value
            .role,
        NumberRole::Quantity
    );
    let bare = only(&format!("2,567 {BAHT}"));
    assert_eq!(bare.value.role, NumberRole::Quantity);
    assert_eq!(bare.value.value, dec("2567"));
}

#[test]
fn ordinals_times_and_codes_are_not_quantities() {
    let role = |text: &str| only(text).value.role;
    assert_eq!(
        role(&format!("ว\u{0E32}ร\u{0E30}{THI} 3")),
        NumberRole::Ordinal
    );
    assert_eq!(role("ข\u{0E49}อ 2.1"), NumberRole::Ordinal);
    assert_eq!(role("หน\u{0E49}\u{0E32} 4"), NumberRole::Ordinal);
    assert_eq!(role("13.30 น."), NumberRole::Time);
    let code = numbers(&format!("{THI} กค 0401/123"));
    assert_eq!(code.len(), 2, "{code:?}");
    assert!(
        code.iter().all(|f| f.value.role == NumberRole::Code),
        "{code:?}"
    );
    // Fix round 2: every code has value 0, so nothing reads it as an amount.
    assert_eq!(
        code.iter().map(|f| f.raw.as_str()).collect::<Vec<_>>(),
        ["0401", "123"]
    );
    assert!(code.iter().all(|f| f.value.value.is_zero()), "{code:?}");
    for text in [
        "No. 5",
        "no.5",
        "#5",
        "Item 3",
        "agenda 2",
        "Section 4.2",
        "PAGE 7",
    ] {
        assert_eq!(role(text), NumberRole::Ordinal, "{text:?}");
    }
    assert_eq!(role("09:05"), NumberRole::Time);
    assert!(only("13:30").flags.contains(Flags::TIME));
    assert_eq!(only("13:30").value.value, dec("13.30"));
    assert_ne!(
        numbers("25:61").first().unwrap().value.role,
        NumberRole::Time
    );
}

#[test]
fn ordinal_marker_does_not_hide_a_figure_with_a_unit() {
    let text = format!("{RAYDAI}รวม{FISCAL} 2568 อย\u{0E39}\u{0E48}{THI} 1,250 {LAN}{BAHT}");
    let found = quantities(&text);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].value.value, dec("1250"));
    assert_eq!(found[0].value.scale, Scale(6));
    assert_eq!(found[0].value.unit, thb());
    assert_eq!(
        only(&format!("ว\u{0E32}ร\u{0E30}{THI} 3")).value.role,
        NumberRole::Ordinal
    );
    let date = format!("{WAN}{THI} 15 ต.ค. 2568");
    assert_eq!(find_dates(&date, &ctx()).unwrap().len(), 1);
    assert!(quantities(&date).is_empty(), "{:?}", numbers(&date));
}

#[test]
fn number_words_inside_ordinary_words_are_not_numbers() {
    for word in [
        "ส\u{0E32}ม\u{0E32}รถ",                               // สามารถ
        "อ\u{0E38}ตส\u{0E32}หกรรม",                           // อุตสาหกรรม
        "\u{0E40}ร\u{0E35}ยบร\u{0E49}อย",                     // เรียบร้อย
        "ส\u{0E32}ม\u{0E35}",                                 // สามี
        "ห\u{0E49}\u{0E32}ม",                                 // ห้าม
        "\u{0E40}ก\u{0E49}\u{0E32}อ\u{0E35}\u{0E49}",         // เก้าอี้
        "ก\u{0E38}มภ\u{0E32}พ\u{0E31}นธ\u{0E4C}",             // กุมภาพันธ์
        "ล\u{0E49}\u{0E32}นน\u{0E32}",                        // ล้านนา
        "จ\u{0E31}งหว\u{0E31}ดร\u{0E49}อย\u{0E40}อ\u{0E47}ด", // จังหวัดร้อยเอ็ด
        // Fix round 1: ranks, place names and other words that hold a power word.
        "พ\u{0E31}นธม\u{0E34}ตร",                                   // พันธมิตร
        "พ\u{0E31}นตร\u{0E35}",                                     // พันตรี
        "พ\u{0E31}น\u{0E42}ท",                                      // พันโท
        "พ\u{0E31}น\u{0E40}อก",                                     // พันเอก
        "พ\u{0E31}นต\u{0E33}รวจ\u{0E40}อก",                         // พันตำรวจเอก
        "พ\u{0E31}นจ\u{0E48}\u{0E32}อ\u{0E32}ก\u{0E32}ศ\u{0E40}อก", // พันจ่าอากาศเอก
        "ร\u{0E49}อยตร\u{0E35}",                                    // ร้อยตรี
        "ร\u{0E49}อย\u{0E42}ท",                                     // ร้อยโท
        "ร\u{0E49}อย\u{0E40}อก",                                    // ร้อยเอก
        "ร\u{0E49}อยต\u{0E33}รวจ\u{0E42}ท",                         // ร้อยตำรวจโท
        "ร\u{0E49}อยกรอง",                                          // ร้อยกรอง
        "ร\u{0E49}อย\u{0E40}ร\u{0E35}ยง",                           // ร้อยเรียง
        "ร\u{0E49}อย\u{0E41}ก\u{0E49}ว",                            // ร้อยแก้ว
        "สร\u{0E49}อยคอ",                                           // สร้อยคอ
        "ส\u{0E34}บตร\u{0E35}",                                     // สิบตรี
        "ส\u{0E34}บ\u{0E42}ท",                                      // สิบโท
        "ส\u{0E34}บ\u{0E40}อก",                                     // สิบเอก
        "ส\u{0E34}บต\u{0E33}รวจตร\u{0E35}",                         // สิบตำรวจตรี
        "\u{0E41}สนส\u{0E38}ข",                                     // แสนสุข
        "ล\u{0E49}\u{0E32}นช\u{0E49}\u{0E32}ง",                     // ล้านช้าง
    ] {
        assert!(numbers(word).is_empty(), "{word:?}: {:?}", numbers(word));
    }
    let percent = only(&format!("{ROI_LA} 12.5"));
    assert_eq!(percent.value.value, dec("12.5"));
    assert_eq!(percent.value.unit, Some(Unit::Percent));
    let text = format!(
        "ค\u{0E48}\u{0E32}\u{0E43}ช\u{0E49}จ\u{0E48}\u{0E32}ย 1,200 {BAHT} ({NUENG}{PHAN}{SONG}{ROI}{BAHT}{THUAN})"
    );
    let found = numbers(&text);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].value.value, dec("1200"));
    assert_eq!(found[0].value.form, Form::Digits);
    assert!(!found[0].flags.contains(Flags::AMOUNT_IN_WORDS));
    assert_eq!(found[1].value.value, dec("1200"));
    assert_eq!(found[1].value.form, Form::Words);
    assert_eq!(found[1].value.role, NumberRole::Quantity);
    assert_eq!(found[1].value.unit, thb());
    assert!(found[1].flags.contains(Flags::AMOUNT_IN_WORDS));
    assert!(found[1].flags.contains(Flags::WORDS));
    assert_eq!(span(&text, &found[1]), format!("{NUENG}{PHAN}{SONG}{ROI}"));
    // A minus before number words counts as before a figure.
    let minus = format!("-{NUENG}{ROI}{BAHT}");
    assert_eq!(only(&minus).value.value, dec("-100"));
    assert_eq!(span(&minus, &only(&minus)), format!("-{NUENG}{ROI}"));
    // Number words followed by a unit are read; without one they are one code (fix round 1).
    assert_eq!(only(&format!("{SAM}{ROI}{BAHT}")).value.value, dec("300"));
    assert_eq!(
        only(&format!("{SAM}{ROI}{KHON}")).value.role,
        NumberRole::Code
    );
    assert!(
        numbers(&format!("{SAM}{BAHT}")).is_empty(),
        "a bare digit word is not a number"
    );
}

#[test]
fn number_span_covers_sign_parentheses_and_scale_but_not_the_unit_word() {
    assert_eq!(span("(1,250)", &only("(1,250)")), "(1,250)");
    assert_eq!(span("-1,250", &only("-1,250")), "-1,250");
    let scaled = format!("1.25 {PHAN}{LAN}{BAHT}");
    assert_eq!(span(&scaled, &only(&scaled)), format!("1.25 {PHAN}{LAN}"));
    assert_eq!(span("12.5%", &only("12.5%")), "12.5%");
    let baht = format!("{RAYDAI} 1,250 {BAHT}");
    assert_eq!(span(&baht, &only(&baht)), "1,250");
    assert_eq!(
        span("THB 1,250 million", &only("THB 1,250 million")),
        "1,250 million"
    );
    assert_eq!(span("$1.5 billion", &only("$1.5 billion")), "$1.5 billion");
    assert_eq!(
        span("1 THB million", &only("1 THB million")),
        "1 THB million"
    );
}

#[test]
fn ranges_lists_and_codes_are_not_negatives_or_thousands() {
    for text in ["10-15", "10\u{2013}15", "10 - 15"] {
        assert_eq!(values(text), vec![dec("10"), dec("15")], "{text:?}");
    }
    assert_eq!(values("2567-2568"), vec![dec("2567"), dec("2568")]);
    let phone = only("000-000-0000");
    assert_eq!(phone.value.role, NumberRole::Code);
    assert_eq!(span("000-000-0000", &phone), "000-000-0000");
    assert_eq!(values("1,2,3"), vec![dec("1"), dec("2"), dec("3")]);
    assert_eq!(values("10,20,30"), vec![dec("10"), dec("20"), dec("30")]);
    assert_eq!(values("1234,567"), vec![dec("1234"), dec("567")]);
    assert_eq!(values("1,2345"), vec![dec("1"), dec("2345")]);
    let marker = only("(1)");
    assert_eq!(marker.value.value, dec("1"));
    assert!(!marker.flags.contains(Flags::PAREN_NEGATIVE));
    assert_eq!(only("(12)").value.value, dec("12"));
    assert_eq!(only("(123)").value.value, dec("-123"));
    assert_eq!(only("(1.5)").value.value, dec("-1.5"));
    // Only bare digits make a list marker; a symbol inside the parentheses makes a negative.
    assert_eq!(only("(5%)").value.value, dec("-5"));
    assert!(only("(5%)").flags.contains(Flags::PAREN_NEGATIVE));
    assert_eq!(only("($12)").value.value, dec("-12"));
    assert_eq!(only("(1,250)").value.value, dec("-1250"));
    for text in ["1.2.3", "192.168.0.1"] {
        let code = only(text);
        assert_eq!(code.value.role, NumberRole::Code, "{text:?}");
        assert_eq!(span(text, &code), text);
    }
}

#[test]
fn english_scale_and_unit_in_either_order() {
    for (text, scale, unit) in [
        ("1 THB million".to_owned(), 6, thb()),
        ("1 million THB".to_owned(), 6, thb()),
        ("THB 1,250 million".to_owned(), 6, thb()),
        ("$1.5 billion".to_owned(), 9, Some(Unit::Currency("USD"))),
        ("USD 2 trillion".to_owned(), 12, Some(Unit::Currency("USD"))),
        ("2 bn".to_owned(), 9, None),
        ("3 mn".to_owned(), 6, None),
        ("4 tn".to_owned(), 12, None),
        ("5 thousand".to_owned(), 3, None),
        (format!("1.25 {PHAN}{LAN}{BAHT}"), 9, thb()),
        ("12.5 baht".to_owned(), 0, thb()),
        ("1,250 million Baht".to_owned(), 6, thb()),
        ("\u{0E3F}1,250".to_owned(), 0, thb()),
    ] {
        let found = only(&text);
        assert_eq!(found.value.scale, Scale(scale), "{text:?}");
        assert_eq!(found.value.unit, unit, "{text:?}");
        assert_eq!(found.value.role, NumberRole::Quantity, "{text:?}");
    }
    // Another dollar is no USD; US$ is.
    assert_eq!(only("S$100").value.unit, None);
    assert_eq!(only("HK$ 100").value.unit, None);
    assert_eq!(only("US$100").value.unit, Some(Unit::Currency("USD")));
    assert_eq!(only("12 bahts").value.unit, None);
    assert_eq!(only("12 Bahtnet").value.unit, None);
    assert_eq!(
        only("2 b").value.scale,
        Scale(0),
        "a single letter is never a scale"
    );
    assert_eq!(only("2 m").value.scale, Scale(0));
}

#[test]
fn mixed_form_is_read() {
    let found = only(&format!("3 {LAN}{HA}{SAEN} {BAHT}"));
    assert_eq!(found.value.form, Form::Mixed);
    assert_eq!(found.value.effective(), dec("3500000"));
    assert_eq!(found.value.unit, thb());
    assert!(found.flags.contains(Flags::WORDS));
}

/// Coordinator ruling 1: a figure with scale and number words is one Mixed number whatever follows it.
#[test]
fn mixed_number_is_read_whatever_follows() {
    let text = format!("{RAYDAI} 3 {LAN}{HA}{SAEN} {KHON}");
    let found = only(&text);
    assert_eq!(found.value.form, Form::Mixed);
    assert_eq!(found.value.effective(), dec("3500000"));
    assert_eq!(span(&text, &found), format!("3 {LAN}{HA}{SAEN}"));
    // The quote "รายได้ 3 ล้าน" cuts this number, so cite widens it and the boundary check refuses it.
    let quote_end = u32::try_from(format!("{RAYDAI} 3 {LAN}").chars().count()).unwrap();
    assert!(
        found.start < quote_end && found.end > quote_end,
        "{found:?}"
    );
    let hundreds = only(&format!("5 {ROI}{BAHT}"));
    assert_eq!(hundreds.value.form, Form::Mixed);
    assert_eq!(hundreds.value.effective(), dec("500"));
    assert_eq!(hundreds.value.unit, thb());
    let thousands = only(&format!("5 {PHAN}{HA}{ROI}{KHON}"));
    assert_eq!(thousands.value.effective(), dec("5500"));
    assert_eq!(thousands.value.scale, Scale(3));
    // A sign stays with the whole Mixed number.
    let negative = only(&format!("-3 {LAN}{HA}{SAEN} {BAHT}"));
    assert_eq!(negative.value.effective(), dec("-3500000"));
    assert_eq!(negative.value.form, Form::Mixed);
    let plain = only(&format!("3 {LAN} {KHON}"));
    assert_eq!(plain.value.form, Form::Digits);
    assert_eq!(plain.value.scale, Scale(6));
}

/// Coordinator ruling 2: a figure Decimal cannot hold is one Code over its whole span, never dropped.
#[test]
fn unholdable_figures_are_codes_over_their_whole_span() {
    let digits = "1234567890".repeat(4);
    let text = format!("{RAYDAI} {digits} {BAHT}");
    let found = only(&text);
    assert_eq!(found.value.role, NumberRole::Code);
    assert_eq!(found.value.value, Decimal::ZERO);
    assert_eq!(found.value.scale, Scale(0));
    assert_eq!(span(&text, &found), digits);
    for text in [
        format!("0.{}", "1".repeat(30)),
        format!("99999999999999999999999999 {LAN}{LAN}"),
        format!("1 {}", LAN.repeat(50)),
        "1".repeat(10_000),
        format!("{} {LAN}", "9".repeat(29)),
    ] {
        let found = only(&text);
        assert_eq!(found.value.role, NumberRole::Code, "{text:?}");
        assert_eq!(found.start, 0, "{text:?}");
        assert_eq!(
            found.end,
            u32::try_from(text.chars().count()).unwrap(),
            "{text:?}"
        );
    }
    let nested = format!("{}1{}", "(".repeat(1_000), ")".repeat(1_000));
    assert_eq!(values(&nested), vec![dec("1")]);
    assert!(numbers(&"(".repeat(100_000)).is_empty());
    // Bare scale words are one code (fix round 1), however many.
    let lans = LAN.repeat(40_000);
    let code = only(&lans);
    assert_eq!(code.value.role, NumberRole::Code);
    assert_eq!(code.end, u32::try_from(lans.chars().count()).unwrap());
}

/// Coordinator ruling 3: Thai letters glued to a figure do not make it a code; other letters do.
#[test]
fn thai_letters_do_not_make_a_code() {
    let count = only(&format!("จ\u{0E33}นวน5{KHON}"));
    assert_eq!(count.value.role, NumberRole::Quantity);
    assert_eq!(count.value.value, dec("5"));
    assert_eq!(only("A1").value.role, NumberRole::Code);
    assert_eq!(only("COVID19").value.role, NumberRole::Code);
    let canary = numbers("KW-CANARY-MINUTES-TH-01");
    assert_eq!(canary.len(), 1);
    assert_eq!(canary[0].value.role, NumberRole::Code);
}

/// Coordinator ruling 8: a scale word that begins an ordinary word is not a scale.
#[test]
fn scale_word_prefix_of_an_ordinary_word_is_not_a_scale() {
    assert_eq!(
        only(&format!("3 {PHAN}ธ\u{0E38}\u{0E4C}")).value.scale,
        Scale(0)
    );
    assert_eq!(only(&format!("5 {LAN}น\u{0E32}")).value.scale, Scale(0));
    assert_eq!(only(&format!("1,250 {LAN}{KHON}")).value.scale, Scale(6));
}

/// Coordinator ruling 8: a figure right after a stray dot is a code, never 5 or 0.5.
#[test]
fn figure_after_a_stray_dot_is_a_code() {
    assert_eq!(only(".5%").value.role, NumberRole::Code);
    assert_eq!(only("x .5 y").value.role, NumberRole::Code);
}

#[test]
fn percent_spellings() {
    for text in [
        "12 percent".to_owned(),
        "12 per cent".to_owned(),
        format!("12 {PERCENT_TH_SHORT}"),
        "12 Percent".to_owned(),
    ] {
        assert_eq!(only(&text).value.unit, Some(Unit::Percent), "{text:?}");
    }
    assert_eq!(only("12 widgets").value.unit, None);
    assert_eq!(only("12 percentile").value.unit, None);
}

#[test]
fn fold_digit_covers_five_scripts() {
    for digit in ['7', '\u{0E57}', '\u{0667}', '\u{06F7}', '\u{FF17}'] {
        assert_eq!(fold_digit(digit), Some(7), "{digit:?}");
    }
    for other in ['\u{00B2}', 'a', '\u{0967}', '\u{2167}', '\u{00BD}', '-'] {
        assert_eq!(fold_digit(other), None, "{other:?}");
    }
    assert_eq!(fold_digit('0'), Some(0));
    assert_eq!(fold_digit('\u{0E59}'), Some(9));
}

#[test]
fn scans_refuse_text_over_the_limit() {
    let fits = format!("{}1,250", " ".repeat(MAX_SCAN_CODE_POINTS - 5));
    assert_eq!(fits.chars().count(), MAX_SCAN_CODE_POINTS);
    assert_eq!(find_numbers(&fits, &ctx()).unwrap().len(), 1);
    assert!(find_dates(&fits, &ctx()).unwrap().is_empty());
    let over = format!("{fits}5");
    let refused = TooLong {
        code_points: MAX_SCAN_CODE_POINTS + 1,
    };
    assert_eq!(find_numbers(&over, &ctx()).unwrap_err(), refused);
    assert_eq!(find_dates(&over, &ctx()).unwrap_err(), refused);
    // The refusal counts the whole text, not one past the limit.
    let longer = format!("{fits}12345");
    let refused = TooLong {
        code_points: MAX_SCAN_CODE_POINTS + 5,
    };
    assert_eq!(find_numbers(&longer, &ctx()).unwrap_err(), refused);
}

#[test]
fn mixed_digits_flagged() {
    let found = only("\u{0E52}\u{0E55}\u{0E56}8");
    assert_eq!(found.value.value, dec("2568"));
    assert!(found.flags.contains(Flags::MIXED_DIGITS));
    assert!(found.flags.contains(Flags::THAI_DIGITS));
    assert!(!only("2568").flags.contains(Flags::MIXED_DIGITS));
}

#[test]
fn numbers_keep_their_original_offsets() {
    let text = "\u{00A0}\u{00A0}\u{200B}\u{0E51},\u{0E52}5\u{0E50}\u{00A0}\u{0E25}\u{0E49}\u{0E32}\u{0E19}";
    let found = only(text);
    assert_eq!((found.start, found.end), (3, 13));
    assert_eq!(found.raw, span(text, &found));
    assert_eq!(found.value.effective(), dec("1250000000"));
    assert_eq!(found.conf, 1.0);
    assert!(found.alts.is_empty());
}

#[test]
fn checked_effective_refuses_what_does_not_fit() {
    let mut number = only("7.5").value;
    assert_eq!(number.checked_effective(), Some(dec("7.5")));
    number.scale = Scale(40);
    assert_eq!(number.checked_effective(), None);
    assert_eq!(number.effective(), Decimal::MAX);
    assert_eq!(Scale(40).multiplier(), Decimal::MAX);
    number.decimals = 40;
    number.scale = Scale(0);
    assert_eq!(number.half_ulp(), Decimal::ZERO);
}

/// SplitMix64, so every random text is the same on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n).unwrap()).unwrap()
    }
}

#[test]
fn scanners_never_panic_and_keep_spans_valid() {
    use unicode_segmentation::UnicodeSegmentation;
    let fragments: Vec<String> = [
        "1",
        "25",
        "2568",
        "1,250",
        ",",
        ".",
        "-",
        "\u{2212}",
        "\u{2013}",
        "/",
        ":",
        "(",
        ")",
        "%",
        "$",
        "\u{0E3F}",
        " ",
        "\u{00A0}",
        "\u{200B}",
        "\u{0E51}\u{0E52}",
        "9999999999999999999999999999999",
        "A",
        "x",
        "q2",
        "FY",
        "BE",
        "AD",
        "No.",
        "#",
        "\u{0E48}",
        "\u{0E31}",
        // Fix round 2: what the round's rules read.
        "\u{FFFD}",
        "\u{0096}",
        "\u{001F}",
        "\u{3164}",
        "O",
        "st",
        "USD",
        "THB",
        "M",
        "mln",
        "1/2",
        "->",
        "and",
        "between",
        "\u{0E16}\u{0E36}\u{0E07}",
        "\u{0E41}\u{0E25}\u{0E30}",
        "\u{0E15}\u{0E34}\u{0E14}\u{0E25}\u{0E1A}",
        "\u{0E19}\u{0E32}\u{0E2C}\u{0E34}\u{0E01}\u{0E32}",
        "\u{0E07}\u{0E27}\u{0E14}",
        "\u{0E15}\u{0E49}\u{0E19}",
        "Jan",
        "q2",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain(
        [
            NUENG, SONG, HA, KAO, SIP, ROI, PHAN, MUEN, SAEN, LAN, LOP, CHUT, BAHT, THUAN, ROI_LA,
            PERCENT_TH, PI, FISCAL, THI,
        ]
        .iter()
        .map(|s| s.to_string()),
    )
    .chain(
        [
            "ต.ค.",
            "ม.ค",
            "กค",
            "ต\u{0E38}ล\u{0E32}คม",
            "พ.ศ.",
            "ร.ศ.",
            "จ.ศ.",
            "\u{0E44}ตรม\u{0E32}ส",
            "may",
            "October",
        ]
        .iter()
        .map(|s| s.to_string()),
    )
    .collect();
    let mut rng = Rng(42);
    for _ in 0..4_000 {
        let pieces = 1 + rng.below(14);
        let text: String = (0..pieces)
            .map(|_| fragments[rng.below(fragments.len())].as_str())
            .collect();
        let len = u32::try_from(text.chars().count()).unwrap();
        let mut bounds: Vec<u32> = text
            .grapheme_indices(true)
            .map(|(at, _)| u32::try_from(text[..at].chars().count()).unwrap())
            .collect();
        bounds.push(len);
        let numbers = find_numbers(&text, &ctx()).unwrap();
        let mut last_end = 0;
        for found in &numbers {
            assert!(
                found.start < found.end && found.end <= len,
                "{text:?}: {found:?}"
            );
            assert!(
                found.start >= last_end,
                "{text:?}: numbers overlap: {numbers:?}"
            );
            assert!(
                bounds.contains(&found.start) && bounds.contains(&found.end),
                "{text:?}: {found:?}"
            );
            assert_eq!(found.raw, span(&text, found), "{text:?}");
            assert!(
                found.value.checked_effective().is_some(),
                "{text:?}: {found:?}"
            );
            last_end = found.end;
        }
        let dates = find_dates(&text, &ctx()).unwrap();
        let mut last_end = 0;
        for found in &dates {
            assert!(
                found.start < found.end && found.end <= len,
                "{text:?}: {found:?}"
            );
            assert!(
                found.start >= last_end,
                "{text:?}: dates overlap: {dates:?}"
            );
            assert!((0.0..=1.0).contains(&found.conf), "{text:?}: {found:?}");
            last_end = found.end;
        }
    }
}

// Fix round 1: the reader fails closed. A figure, a sign or number words it cannot place are one
// Code over the whole run, or a reading below 0.8 with the other reading in `alts`.

const LAI: &str = "หล\u{0E32}ย"; // หลาย
const NAP: &str = "น\u{0E31}บ"; // นับ
const KHRUENG: &str = "คร\u{0E36}\u{0E48}ง"; // ครึ่ง
const TID_LOP: &str = "ต\u{0E34}ดลบ"; // ติดลบ
const KAO_EE: &str = "\u{0E40}ก\u{0E49}\u{0E32}อ\u{0E35}\u{0E49}"; // เก้าอี้
const SAMAT: &str = "ส\u{0E32}ม\u{0E32}รถ"; // สามารถ
const HAM: &str = "ห\u{0E49}\u{0E32}ม"; // ห้าม
const BUDGET: &str = "งบปร\u{0E30}ม\u{0E32}ณ"; // งบประมาณ
const WELA: &str = "\u{0E40}วล\u{0E32}"; // เวลา
const GROWTH: &str = "อ\u{0E31}ตร\u{0E32}\u{0E40}ต\u{0E34}บ\u{0E42}ต"; // อัตราเติบโต
const KHAT_THUN: &str = "ข\u{0E32}ดท\u{0E38}น"; // ขาดทุน
const KAMRAI: &str = "ก\u{0E33}\u{0E44}ร"; // กำไร
const RAKHA: &str = "ร\u{0E32}ค\u{0E32}"; // ราคา

fn effective(text: &str) -> Vec<Decimal> {
    quantities(text)
        .iter()
        .map(|found| found.value.effective())
        .collect()
}

/// Fix round 1: a character step 1 now removes, a separator the grammar does not read, a digit
/// the fold does not cover, Arabic-Indic digits beside another script, or a stray comma never
/// splits one visible figure into a head and a confident tail.
#[test]
fn unknown_separators_and_unfolded_digits_never_split_a_figure() {
    for invisible in ['\u{2063}', '\u{200E}', '\u{FE0F}', '\u{FFF9}', '\u{E0100}'] {
        let text = format!("{RAYDAI} 1{invisible}250 {LAN}{BAHT}");
        let found = only(&text);
        assert_eq!(found.value.effective(), dec("1250000000"), "{text:?}");
    }
    // Fix round 2: a control or a Hangul filler is seen (a mark or a gap), so it makes a code.
    for separator in [
        '\u{2009}', '\u{200A}', '\u{202F}', '\u{2007}', '\u{2008}', '\u{FE50}', '\u{FE52}',
        '\u{201A}', '\u{2027}', '\u{00B7}', '\'', '\u{2019}', '\u{02BC}', '\u{066B}', '\u{066C}',
        '\u{0001}', '\u{0096}', '\u{3164}', '\u{FFA0}', '\u{115F}',
    ] {
        let text = format!("{RAYDAI} 1{separator}250 {LAN}{BAHT}");
        let found = only(&text);
        assert_eq!(found.value.role, NumberRole::Code, "{text:?}");
        assert_eq!(
            span(&text, &found),
            format!("1{separator}250 {LAN}"),
            "{text:?}"
        );
    }
    for head in [
        "\u{0ED1},\u{0E52}\u{0E55}\u{0E50}",
        "\u{1D7E3},250",
        "\u{0967},250",
        "\u{00B9},250",
        "x,250",
        "l,250",
        "1\u{0660}250",
        "\u{1D7E3}250",
    ] {
        let text = format!("{RAYDAI} {head} {LAN}{BAHT}");
        let code = only(&text);
        assert_eq!(code.value.role, NumberRole::Code, "{text:?}");
        assert_eq!(span(&text, &code), format!("{head} {LAN}"), "{text:?}");
    }
    for digits in [
        "\u{0967}\u{0968}\u{0969}",
        "\u{0ED1}\u{0ED2}\u{0ED3}",
        "\u{1D7CF}\u{1D7D0}\u{1D7D1}",
    ] {
        let text = format!("{RAKHA} {digits} {BAHT}");
        let code = only(&text);
        assert_eq!(code.value.role, NumberRole::Code, "{text:?}");
        assert_eq!(span(&text, &code), digits, "{text:?}");
    }
    // A decimal part followed by a comma group is one code, scale word and all.
    let european = format!("ยอดข\u{0E32}ย 1.234,5 {LAN}");
    let code = only(&european);
    assert_eq!(code.value.role, NumberRole::Code);
    assert_eq!(span(&european, &code), format!("1.234,5 {LAN}"));
    // Thai digits beside ASCII ones are still read, with MIXED_DIGITS.
    let mixed = only("\u{0E51},250");
    assert_eq!(mixed.value.value, dec("1250"));
    assert!(mixed.flags.contains(Flags::MIXED_DIGITS));
}

/// Fix round 1: step 3 folds the hyphens and minus signs that look like `-`, so each is a minus.
#[test]
fn dash_like_minus_signs_make_a_figure_negative() {
    for dash in [
        '\u{2010}', '\u{2011}', '\u{FE63}', '\u{02D7}', '\u{2796}', '\u{2212}', '\u{2012}',
        '\u{2013}', '\u{FF0D}', '-',
    ] {
        let text = format!("{KAMRAI} {dash}1,250 {LAN}{BAHT}");
        let found = only(&text);
        assert_eq!(found.value.value, dec("-1250"), "{text:?}");
        assert_eq!(
            span(&text, &found),
            format!("{dash}1,250 {LAN}"),
            "{text:?}"
        );
    }
}

/// Fix round 1: a dash touching its figure is a minus even when another figure stands one space
/// before it; a dash right after a figure, or with spaces on both sides, joins a range.
#[test]
fn a_dash_touching_its_figure_is_a_minus() {
    let text = format!("{GROWTH}{PI} 2567 -5%");
    let found = quantities(&text);
    assert_eq!(found.len(), 1, "{:?}", numbers(&text));
    assert_eq!(found[0].value.value, dec("-5"));
    assert_eq!(span(&text, &found[0]), "-5%");
    for text in [
        "growth 2024 -5%",
        "growth 2024 \u{2212}5%",
        "revenue 1,250 -5%",
    ] {
        assert_eq!(values(text).last(), Some(&dec("-5")), "{text:?}");
    }
    assert_eq!(
        values("net profit 120 -45 -300"),
        vec![dec("120"), dec("-45"), dec("-300")]
    );
    assert_eq!(values("10 -15"), vec![dec("10"), dec("-15")]);
    for text in [
        "10-15".to_owned(),
        "10 - 15".to_owned(),
        "10\u{2013}15".to_owned(),
        "5%-3%".to_owned(),
        format!("5%-{NUENG}{ROI}{BAHT} 7"),
    ] {
        let text = text.as_str();
        assert!(
            values(text).iter().all(|value| !value.is_sign_negative()),
            "{text:?}: {:?}",
            numbers(text)
        );
    }
}

/// Fix round 1: ติดลบ and `minus` before a figure make it negative and belong to its span; a
/// bare ลบ, which also means "delete", leaves it positive at 0.5 with the negative reading in
/// its alternatives.
#[test]
fn sign_words_make_a_figure_negative_or_ambiguous() {
    let economy = format!("\u{0E40}ศรษฐก\u{0E34}จ{TID_LOP}{ROI_LA} 6.1");
    let found = only(&economy);
    assert_eq!(found.value.value, dec("-6.1"));
    assert_eq!(found.value.unit, Some(Unit::Percent));
    assert_eq!(span(&economy, &found), format!("{TID_LOP}{ROI_LA} 6.1"));
    let loss = format!("ผลปร\u{0E30}กอบก\u{0E32}ร{TID_LOP} 5 {LAN}{BAHT}");
    let found = only(&loss);
    assert_eq!(found.value.effective(), dec("-5000000"));
    assert_eq!(span(&loss, &found), format!("{TID_LOP} 5 {LAN}"));
    let english = "GDP contracted minus 2.5 percent";
    let found = only(english);
    assert_eq!(found.value.value, dec("-2.5"));
    assert_eq!(span(english, &found), "minus 2.5");
    assert_eq!(only("terminus 5").value.value, dec("5"));
    let bare = format!("{LOP} 5 {LAN}{BAHT}");
    let found = only(&bare);
    assert_eq!(found.value.effective(), dec("5000000"));
    assert_eq!(found.conf, 0.5);
    assert_eq!(found.alts.len(), 1);
    assert_eq!(found.alts[0].0.effective(), dec("-5000000"));
    assert_eq!(found.alts[0].1, 0.5);
    assert_eq!(span(&bare, &found), format!("{LOP} 5 {LAN}"));
    let words = format!("{TID_LOP} {HA}{ROI}{BAHT}");
    assert_eq!(only(&words).value.value, dec("-500"));
    assert_eq!(span(&words, &only(&words)), format!("{TID_LOP} {HA}{ROI}"));
    let glued = format!("{LOP}{HA}{ROI}{BAHT}");
    let found = only(&glued);
    assert_eq!(found.value.value, dec("500"));
    assert_eq!(found.conf, 0.5);
    assert_eq!(found.alts[0].0.value, dec("-500"));
}

/// Fix round 1: Latin (or other non-Thai) letters glued to a figure make it one code over the
/// figure and the letters, whatever unit stands before it; an English scale word glued on is
/// still a scale, and letters after a symbol unit are not glued to the figure.
#[test]
fn letters_glued_to_a_figure_make_one_code_over_both() {
    for (text, code) in [
        ("fee $5M", "$5M"),
        ("revenue $1.2B", "$1.2B"),
        ("salary $50K", "$50K"),
        ("revenue \u{0E3F}10M", "\u{0E3F}10M"),
        ("revenue THB 5M", "5M"),
        ("THB 250M", "250M"),
        ("$1.5B", "$1.5B"),
        ("US$1.5M", "US$1.5M"),
        ("revenue 5M", "5M"),
        ("5kg", "5kg"),
        ("COVID19", "COVID19"),
        ("5\u{03B1}", "5\u{03B1}"),
        ("\u{0E81}5", "\u{0E81}5"),
    ] {
        let found = only(text);
        assert_eq!(found.value.role, NumberRole::Code, "{text:?}");
        assert_eq!(span(text, &found), code, "{text:?}");
    }
    for (text, scale, unit) in [
        ("1.25bn", 9, None),
        ("USD 3.2bn", 9, Some(Unit::Currency("USD"))),
        ("2 bn", 9, None),
        ("\u{0E3F}1,250", 0, thb()),
        ("12.5%YoY", 0, Some(Unit::Percent)),
    ] {
        let found = only(text);
        assert_eq!(found.value.role, NumberRole::Quantity, "{text:?}");
        assert_eq!(found.value.scale, Scale(scale), "{text:?}");
        assert_eq!(found.value.unit, unit, "{text:?}");
    }
    let section = only("section 3B");
    assert_eq!(section.value.role, NumberRole::Ordinal);
    assert_eq!(span("section 3B", &section), "3B");
    let item = only("item5");
    assert_eq!(item.value.role, NumberRole::Ordinal);
    assert_eq!(span("item5", &item), "5");
    // US$ is one symbol: its letters are no letters glued to a code.
    let dollars = only("US$100");
    assert_eq!(dollars.value.role, NumberRole::Quantity);
    assert_eq!(span("US$100", &dollars), "US$100");
}

/// Fix round 1: a currency word or symbol that a figure follows belongs to that figure, a unit
/// before a figure wins over one after it, and a symbol one figure took is not the next one's.
#[test]
fn a_unit_before_the_next_figure_is_not_this_figures_unit() {
    let usd = Some(Unit::Currency("USD"));
    let units = |text: &str| {
        numbers(text)
            .into_iter()
            .map(|found| found.value.unit)
            .collect::<Vec<_>>()
    };
    assert_eq!(units("USD 100 THB 3,500"), vec![usd.clone(), thb()]);
    assert_eq!(
        units("THB 1,250 million USD 35 million"),
        vec![thb(), usd.clone()]
    );
    assert_eq!(units("5 $10"), vec![None, usd.clone()]);
    assert_eq!(units("5\u{0E3F} 10"), vec![thb(), None]);
    assert_eq!(units("USD 100, THB 3,500"), vec![usd.clone(), thb()]);
    assert_eq!(units("USD 100 THB"), vec![usd]);
    assert_eq!(units("1 THB million"), vec![thb()]);
}

/// Fix round 1: one or two digits in parentheses before a scale word or a unit are an
/// accounting negative; before the next figure's currency they are still a list marker.
#[test]
fn list_marker_before_a_scale_or_unit_is_an_accounting_negative() {
    for (digits, value) in [("5", "-5"), ("12", "-12")] {
        let text = format!("{KHAT_THUN} ({digits}) {LAN}{BAHT}");
        let found = only(&text);
        assert_eq!(found.value.value, dec(value), "{text:?}");
        assert_eq!(found.value.scale, Scale(6), "{text:?}");
        assert_eq!(found.value.unit, thb(), "{text:?}");
        assert!(found.flags.contains(Flags::PAREN_NEGATIVE), "{text:?}");
        assert_eq!(span(&text, &found), format!("({digits}) {LAN}"), "{text:?}");
    }
    assert_eq!(only(&format!("(5) {BAHT}")).value.value, dec("-5"));
    assert_eq!(
        values("(1) USD 500 registration fee"),
        vec![dec("1"), dec("500")]
    );
    assert_eq!(only("(1)").value.value, dec("1"));
}

/// Fix round 1: number words that start with a power word or ล้าน (หลายล้าน, ครึ่งล้าน, a unit
/// header) are one code, never an amount; number words that start with a digit word keep their
/// reading.
#[test]
fn bare_power_words_are_codes_never_amounts() {
    for (text, code) in [
        (
            format!("\u{0E43}ช\u{0E49}งบ{LAI}{LAN}{BAHT}"),
            LAN.to_owned(),
        ),
        (
            format!("\u{0E43}ช\u{0E49}งบ{LAI}{ROI}{BAHT}"),
            ROI.to_owned(),
        ),
        (
            format!("\u{0E40}ส\u{0E35}ยห\u{0E32}ย{NAP}{PHAN}{LAN}{BAHT}"),
            format!("{PHAN}{LAN}"),
        ),
        (
            format!("{BUDGET}{KHRUENG}{LAN}{BAHT}"),
            format!("{KHRUENG}{LAN}"),
        ),
        (
            format!("ลงท\u{0E38}น{LAI}{ROI}{LAN}{BAHT}"),
            format!("{ROI}{LAN}"),
        ),
        (format!("หน\u{0E48}วย: {LAN}{BAHT}"), LAN.to_owned()),
        (
            format!("ม\u{0E39}ลค\u{0E48}\u{0E32}{NAP}{PHAN}{LAN}{BAHT}"),
            format!("{PHAN}{LAN}"),
        ),
    ] {
        let found = only(&text);
        assert_eq!(found.value.role, NumberRole::Code, "{text:?}");
        assert_eq!(span(&text, &found), code, "{text:?}");
    }
    let header = format!("{RAYDAI} ({LAN}{BAHT}) 1,250");
    assert_eq!(
        effective(&header),
        vec![dec("1250")],
        "{:?}",
        numbers(&header)
    );
    let after = format!("{RAYDAI} 1,250 ({LAN}{BAHT})");
    let found = numbers(&after);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].value.value, dec("1250"));
    assert_eq!(found[1].value.role, NumberRole::Code);
    assert!(!found[1].flags.contains(Flags::AMOUNT_IN_WORDS));
    assert_eq!(
        only(&format!("{NUENG}{LAN}{BAHT}")).value.value,
        dec("1000000")
    );
    assert_eq!(
        only(&format!("1 {LAN}{BAHT}")).value.effective(),
        dec("1000000")
    );
}

/// Fix round 1: number words with a power word and no unit after them are one code, so no quote
/// or claim can end inside them; right after ร้อยละ a numeral alone is a percent.
#[test]
fn number_words_without_a_unit_are_one_code() {
    for text in [
        format!("{RAKHA} {NUENG}{LAN}{SONG}{SAEN} {KHON}"),
        format!("ปร\u{0E30}ช\u{0E32}กร{NUENG}{LAN}{SONG}{SAEN}{KHON}"),
    ] {
        let found = only(&text);
        assert_eq!(found.value.role, NumberRole::Code, "{text:?}");
        assert_eq!(
            span(&text, &found),
            format!("{NUENG}{LAN}{SONG}{SAEN}"),
            "{text:?}"
        );
    }
    for (text, value) in [
        (format!("{ROI_LA}{HA}"), "5"),
        (format!("{ROI_LA}{HA}{SIP}"), "50"),
        (format!("{ROI_LA}{ROI}"), "100"),
        (format!("{ROI_LA} {HA}{CHUT}{HA}"), "5.5"),
    ] {
        let found = only(&text);
        assert_eq!(found.value.value, dec(value), "{text:?}");
        assert_eq!(found.value.unit, Some(Unit::Percent), "{text:?}");
        assert_eq!(found.value.role, NumberRole::Quantity, "{text:?}");
    }
    // Thirty percent were able: สาม begins สามารถ, so the words are no 33%.
    let able = format!("{ROI_LA}{SAM}{SIP}{SAMAT}");
    assert!(quantities(&able).is_empty(), "{:?}", numbers(&able));
    let spaced = only(&format!("{HA}{SIP} {BAHT}"));
    assert_eq!(spaced.value.value, dec("50"));
    assert_eq!(spaced.value.unit, thb());
    assert!(numbers(&format!("{SAM}{BAHT}")).is_empty());
    // The oracle reads each word after จุด alone, so สามจุดห้าล้าน would be 10,003.5: a code.
    let point = format!("{SAM}{CHUT}{HA}{LAN}{BAHT}");
    assert_eq!(only(&point).value.role, NumberRole::Code);
    // The grammar multiplies the first part again where a reader may mean a sum: a code.
    let regrouped = format!("{SAM}{PHAN}{LAN}{HA}{ROI}{LAN}{BAHT}");
    assert_eq!(only(&regrouped).value.role, NumberRole::Code);
    let multiplied = format!("{HA}{LAN}{SONG}{SAEN}{LAN}{BAHT}");
    assert_eq!(only(&multiplied).value.effective(), dec("5200000000000"));
}

/// Fix round 1: a figure with its scale, then a figure or digit-led number words with a smaller
/// scale of their own, is one Mixed number; a run whose later part does not fit below the
/// earlier one, or that the grammar reads otherwise, is one code; a figure with no scale word of
/// its own never joins.
#[test]
fn compound_amounts_join_or_become_one_code() {
    for (text, amount, unit, written) in [
        (
            format!("{RAYDAI} 3 {LAN} 9 {SAEN}{BAHT}"),
            "3900000",
            thb(),
            format!("3 {LAN} 9 {SAEN}"),
        ),
        (
            format!("{RAKHA} 1{LAN}2{SAEN}{BAHT}"),
            "1200000",
            thb(),
            format!("1{LAN}2{SAEN}"),
        ),
        (
            format!("{RAYDAI} 3 {LAN} {KAO}{SAEN}{BAHT}"),
            "3900000",
            thb(),
            format!("3 {LAN} {KAO}{SAEN}"),
        ),
        (
            format!("2 {PHAN} 5 {ROI} {BAHT}"),
            "2500",
            thb(),
            format!("2 {PHAN} 5 {ROI}"),
        ),
        (
            format!("3 {SAEN} 5 {MUEN}{BAHT}"),
            "350000",
            thb(),
            format!("3 {SAEN} 5 {MUEN}"),
        ),
        (
            format!("5 {LAN} 3 {MUEN}{KHON}"),
            "5030000",
            None,
            format!("5 {LAN} 3 {MUEN}"),
        ),
        (
            format!("3 {LAN} {HA}{SAEN} {BAHT}"),
            "3500000",
            thb(),
            format!("3 {LAN} {HA}{SAEN}"),
        ),
        (
            format!("3.5 {LAN} 2 {MUEN}{BAHT}"),
            "3520000",
            thb(),
            format!("3.5 {LAN} 2 {MUEN}"),
        ),
        (
            "3 million 500 thousand".to_owned(),
            "3500000",
            None,
            "3 million 500 thousand".to_owned(),
        ),
    ] {
        let found = only(&text);
        assert_eq!(found.value.effective(), dec(amount), "{text:?}");
        assert_eq!(found.value.form, Form::Mixed, "{text:?}");
        assert_eq!(found.value.unit, unit, "{text:?}");
        assert_eq!(span(&text, &found), written, "{text:?}");
    }
    let compound = only(&format!("3 {LAN} 9 {SAEN}{BAHT}"));
    assert_eq!(compound.value.half_ulp(), dec("50000"));
    let pieces = format!("ซ\u{0E37}\u{0E49}อ 3 {LAN} 5 ช\u{0E34}\u{0E49}น");
    assert_eq!(effective(&pieces), vec![dec("3000000"), dec("5")]);
    for text in [
        format!("3 {LAN} 5 {LAN}{BAHT}"),
        format!("3 {LAN} 12 {SAEN}{BAHT}"),
        format!("3.5 {LAN} 2 {SAEN}{BAHT}"),
        format!("3 {PHAN}{LAN} 5 {ROI}{LAN}{BAHT}"),
        format!("3 {SAEN} 5 {SAEN}{BAHT}"),
        format!("3 {LAN}{SAEN}{BAHT}"),
    ] {
        let found = only(&text);
        assert_eq!(found.value.role, NumberRole::Code, "{text:?}");
        assert_eq!(found.start, 0, "{text:?}");
    }
}

/// Fix round 1: ครึ่ง (half) beside a scale word makes the run one code; it is not in the grammar.
#[test]
fn half_beside_a_scale_word_is_one_code() {
    for (text, code) in [
        (
            format!("{RAKHA} 3 {LAN}{KHRUENG}{BAHT}"),
            format!("3 {LAN}{KHRUENG}"),
        ),
        (
            format!("3 {SAEN}{KHRUENG}{BAHT}"),
            format!("3 {SAEN}{KHRUENG}"),
        ),
        (format!("{KHRUENG}{LAN}{BAHT}"), format!("{KHRUENG}{LAN}")),
        (
            format!("{NUENG}{LAN}{KHRUENG}{BAHT}"),
            format!("{NUENG}{LAN}{KHRUENG}"),
        ),
    ] {
        let found = only(&text);
        assert_eq!(found.value.role, NumberRole::Code, "{text:?}");
        assert_eq!(span(&text, &found), code, "{text:?}");
    }
}

/// Fix round 1: a Mixed number or number words that end in a digit word a Thai letter continues
/// (เก้าอี้, ห้าม, สามารถ) are one code: the reader cannot tell the number from the word.
#[test]
fn a_digit_word_that_starts_an_ordinary_word_makes_a_code() {
    for text in [
        format!("จ\u{0E31}ดซ\u{0E37}\u{0E49}อ 5 {PHAN}{KAO_EE}"),
        format!("งบ 3 {LAN}{HAM}จ\u{0E48}\u{0E32}ย"),
        format!("5 {PHAN}{SAMAT}"),
    ] {
        let found = numbers(&text);
        assert_eq!(found.len(), 1, "{text:?}: {found:?}");
        assert_eq!(found[0].value.role, NumberRole::Code, "{text:?}");
    }
    assert_eq!(
        only(&format!("3 {LAN}{HA}{SAEN} {KHON}")).value.effective(),
        dec("3500000")
    );
    assert_eq!(
        only(&format!("3 {LAN}{HA}{BAHT}")).value.effective(),
        dec("3000005")
    );
    assert_eq!(
        only(&format!("5 {ROI}{BAHT}")).value.effective(),
        dec("500")
    );
}

/// Fix round 1 (and coordinator ruling 8): a rank or place name that starts with a power word is
/// no scale after a figure.
#[test]
fn rank_and_place_words_are_not_scales() {
    for (figure, word) in [
        ("3", "พ\u{0E31}นตร\u{0E35}"),
        ("5", "พ\u{0E31}น\u{0E42}ท"),
        ("2", "พ\u{0E31}น\u{0E40}อก"),
        ("1", "ล\u{0E49}\u{0E32}นช\u{0E49}\u{0E32}ง"),
        ("1", "พ\u{0E31}นต\u{0E33}รวจ\u{0E40}อก"),
        ("2", "ร\u{0E49}อยต\u{0E33}รวจ\u{0E40}อก"),
        ("3", "ส\u{0E34}บต\u{0E33}รวจ\u{0E42}ท"),
        ("4", "ร\u{0E49}อย\u{0E42}ท"),
        ("5", "ส\u{0E34}บ\u{0E40}อก"),
        ("8", "\u{0E41}สนส\u{0E38}ข"),
    ] {
        let text = format!("{figure} {word}");
        let found = only(&text);
        assert_eq!(found.value.scale, Scale(0), "{text:?}");
        assert_eq!(found.value.form, Form::Digits, "{text:?}");
        assert_eq!(found.value.value, dec(figure), "{text:?}");
    }
}

/// Fix round 1: a time with seconds is one Time, a field after another clock field starts no
/// clock, and the first time of a Thai range takes the น. of the second; hours stop at 24 and
/// minutes at 59.
#[test]
fn times_with_seconds_and_thai_time_ranges() {
    let start = "ปร\u{0E30}ช\u{0E38}ม\u{0E40}ร\u{0E34}\u{0E48}ม 09:30:00 น.";
    let found = only(start);
    assert_eq!(found.value.role, NumberRole::Time);
    assert_eq!(span(start, &found), "09:30:00");
    assert_eq!(only("9:05:15").value.role, NumberRole::Time);
    assert_eq!(only("9:05:15.250").value.role, NumberRole::Time);
    for text in ["13:60", "25:30", "25.30 น.", "10:20:30:40", "9:05:75"] {
        assert!(
            numbers(text)
                .iter()
                .all(|found| found.value.role != NumberRole::Time),
            "{text:?}: {:?}",
            numbers(text)
        );
    }
    for text in [
        format!("{WELA} 09.00 - 12.00 น."),
        format!("{WELA} 13.30-16.30 น."),
        format!("{WELA} 09.00 ถ\u{0E36}ง 12.00 น."),
    ] {
        let found = numbers(&text);
        assert_eq!(found.len(), 2, "{text:?}: {found:?}");
        assert!(
            found
                .iter()
                .all(|found| found.value.role == NumberRole::Time),
            "{text:?}: {found:?}"
        );
    }
}

/// Fix round 1 (mutant M2): the amount-in-words pattern needs its closing parenthesis.
#[test]
fn amount_in_words_needs_its_closing_parenthesis() {
    let text = format!("1,200 {BAHT} ({NUENG}{PHAN}{SONG}{ROI}{BAHT}");
    assert!(
        numbers(&text)
            .iter()
            .all(|found| !found.flags.contains(Flags::AMOUNT_IN_WORDS)),
        "{:?}",
        numbers(&text)
    );
}

/// Fix round 1 (mutant U15): an English scale word ends at a word boundary.
#[test]
fn english_scales_end_at_a_word_boundary() {
    for text in ["3 millionaires", "2 thousands", "4 billionth"] {
        let found = only(text);
        assert_eq!(found.value.scale, Scale(0), "{text:?}");
        assert_eq!(found.value.role, NumberRole::Quantity, "{text:?}");
    }
}

/// Fix round 1 (mutants U09, E09 and E20): 28 decimals are a code, a Mixed number after a figure
/// with decimals is a code unless its words fit below the figure's last place, and a number in
/// words keeps its decimal places.
#[test]
fn decimal_limits_and_word_precision() {
    let long = format!("0.{}", "1".repeat(28));
    assert_eq!(only(&long).value.role, NumberRole::Code);
    let fits = format!("0.{}", "1".repeat(27));
    assert_eq!(only(&fits).value.role, NumberRole::Quantity);
    assert_eq!(
        only(&format!("0.5 {PHAN}{HA}{ROI} {KHON}")).value.role,
        NumberRole::Code
    );
    assert_eq!(
        only(&format!("2.5 {ROI} {KHON}")).value.effective(),
        dec("250")
    );
    // A figure before a power word counts as written, even below one.
    assert_eq!(
        only(&format!("0.5 {ROI} {KHON}")).value.effective(),
        dec("50")
    );
    let words = only(&format!("{NUENG}{ROI}{CHUT}{HA}{BAHT}"));
    assert_eq!(words.value.value, dec("100.5"));
    assert_eq!(words.value.decimals, 1);
}

/// Fix round 1 (mutant M13): a Mixed number too large for a Decimal is one code over its span.
#[test]
fn overflowing_mixed_number_is_one_code() {
    let text = format!("99999999999999999999999999 {LAN}{HA}{SAEN}");
    let found = only(&text);
    assert_eq!(found.value.role, NumberRole::Code);
    assert_eq!(span(&text, &found), text);
}

// Fix round 2: the clean-context rule (a founder decision). A quantity reads at 1.0 only when it
// matches a pattern the reader knows exactly and no signal of a range, a list, a fraction or a
// sign stands near it; otherwise at most 0.6, with the readings the signal suggests in `alts`.
// What the reader cannot read at all is one code.

const THUENG: &str = "ถ\u{0E36}ง"; // ถึง
const LAE: &str = "\u{0E41}ล\u{0E30}"; // และ
const RAWANG: &str = "ร\u{0E30}หว\u{0E48}\u{0E32}ง"; // ระหว่าง
const YU_THI: &str = "อย\u{0E39}\u{0E48}ท\u{0E35}\u{0E48}"; // อยู่ที่
const PRAMAN: &str = "ปร\u{0E30}ม\u{0E32}ณ"; // ประมาณ
const SETTHAKIT: &str = "\u{0E40}ศรษฐก\u{0E34}จ"; // เศรษฐกิจ
const KHAYAI: &str = "ขย\u{0E32}ยต\u{0E31}ว"; // ขยายตัว
const PHOEM: &str = "\u{0E40}พ\u{0E34}\u{0E48}มข\u{0E36}\u{0E49}น"; // เพิ่มขึ้น
const PANPHON: &str = "ป\u{0E31}นผล"; // ปันผล
const PER_SHARE: &str = "บ\u{0E32}ทต\u{0E48}อห\u{0E38}\u{0E49}น"; // บาทต่อหุ้น
const FEE: &str = "ค\u{0E48}\u{0E32}ธรรม\u{0E40}น\u{0E35}ยม"; // ค่าธรรมเนียม
const SALARY: &str = "\u{0E40}ง\u{0E34}น\u{0E40}ด\u{0E37}อน"; // เงินเดือน
const NALIKA: &str = "น\u{0E32}ฬ\u{0E34}ก\u{0E32}"; // นาฬิกา
const SUE: &str = "ซ\u{0E37}\u{0E49}อ"; // ซื้อ
const CHIN: &str = "ช\u{0E34}\u{0E49}น"; // ชิ้น
const YOTKHAI: &str = "ยอดข\u{0E32}ย"; // ยอดขาย
const KHA_CHAI: &str = "ค\u{0E48}\u{0E32}\u{0E43}ช\u{0E49}จ\u{0E48}\u{0E32}ย"; // ค่าใช้จ่าย
const PHONPRAKOP: &str = "ผลปร\u{0E30}กอบก\u{0E32}ร"; // ผลประกอบการ
const TAE: &str = "\u{0E41}ต\u{0E48}"; // แต่
const CHAMNUAN: &str = "จ\u{0E33}นวน"; // จำนวน
const SUNG_THUENG: &str = "ส\u{0E39}งถ\u{0E36}ง"; // สูงถึง
const KUEAP: &str = "\u{0E40}ก\u{0E37}อบ"; // เกือบ
const NGOEN_FOE: &str = "\u{0E40}ง\u{0E34}น\u{0E40}ฟ\u{0E49}อ"; // เงินเฟ้อ
const ET: &str = "\u{0E40}อ\u{0E47}ด"; // เอ็ด
const PHANTRI: &str = "พ\u{0E31}นตร\u{0E35}"; // พันตรี

/// The values of the quantities read at 0.8 or more.
fn confident(text: &str) -> Vec<Decimal> {
    quantities(text)
        .iter()
        .filter(|found| found.conf >= 0.8)
        .map(|found| found.value.effective())
        .collect()
}

/// Fix round 2 (a regression of round 1): a control step 1 deletes is seen as a mark or a gap,
/// so inside or right before a figure it makes the run one code, never a join of two figures or
/// a dropped minus; the information separators U+001C-U+001F are spaces and keep figures apart.
#[test]
fn controls_inside_a_figure_make_a_code_and_separators_keep_figures_apart() {
    // U+0096 is an en dash decoded with the wrong code page.
    for (text, code) in [
        (
            format!("{YOTKHAI} 10\u{0096}15 {LAN}{BAHT}"),
            format!("10\u{0096}15 {LAN}"),
        ),
        (format!("{GROWTH} \u{0096}5%"), "\u{0096}5%".to_owned()),
        (
            format!("{PI} 2567\u{0096}2569"),
            "2567\u{0096}2569".to_owned(),
        ),
        (
            format!("{PI} 2567\u{0007}2568"),
            "2567\u{0007}2568".to_owned(),
        ),
        (
            format!("{RAYDAI} 1\u{0001}250 {LAN}{BAHT}"),
            format!("1\u{0001}250 {LAN}"),
        ),
        (
            format!("{RAYDAI} 1\u{3164}250 {LAN}{BAHT}"),
            format!("1\u{3164}250 {LAN}"),
        ),
    ] {
        let found = only(&text);
        assert_eq!(found.value.role, NumberRole::Code, "{text:?}");
        assert_eq!(span(&text, &found), code, "{text:?}");
    }
    let year = format!("{PI} 2567\u{001F}1,250 {LAN}{BAHT}");
    assert_eq!(confident(&year), vec![dec("1250000000")]);
    assert_eq!(values("1,250\u{001F}300"), vec![dec("1250"), dec("300")]);
    assert_eq!(values("100\u{001C}200"), vec![dec("100"), dec("200")]);
}

/// Fix round 2: digits joined by a code point that is no boundary the reader knows are one
/// code, by a rule rather than a list, U+FFFD first; a glyph the text layer could not name
/// touching a figure makes it a code too.
#[test]
fn separators_nobody_listed_never_split_a_figure() {
    for separator in [
        '\u{FFFD}',
        '\u{FFFC}',
        '\u{E000}',
        '\u{F0000}',
        '\u{00B8}',
        '\u{2024}',
        '\u{2219}',
        '\u{22C5}',
        '\u{0326}',
        '\u{0332}',
        '\u{0E48}',
        '\u{0E31}',
        '\u{2E31}',
        '\u{30FB}',
        '\u{FF65}',
        '\u{2032}',
        '`',
        '\u{00B4}',
        '\u{2018}',
        '\u{02CC}',
        '\u{3001}',
        '\u{060C}',
        '\u{FF64}',
        '\u{2026}',
        '\u{2044}',
        '\u{2215}',
        '\u{00D7}',
    ] {
        let text = format!("{RAYDAI} 1{separator}250 {LAN}{BAHT}");
        let found = only(&text);
        assert_eq!(found.value.role, NumberRole::Code, "{text:?}");
        assert_eq!(
            span(&text, &found),
            format!("1{separator}250 {LAN}"),
            "{text:?}"
        );
    }
    for written in ["\u{FFFD},250", "1,\u{FFFD}50", "\u{FFFD}250", "250\u{FFFD}"] {
        let text = format!("{RAYDAI} {written} {LAN}{BAHT}");
        let found = only(&text);
        assert_eq!(found.value.role, NumberRole::Code, "{text:?}");
        assert_eq!(span(&text, &found), format!("{written} {LAN}"), "{text:?}");
    }
    // Two glyphs in a row: each code takes only its own (round 2's random-text harness).
    let pair = numbers("1\u{FFFD}\u{FFFD}-5");
    assert_eq!(pair.len(), 2, "{pair:?}");
    assert!(pair[0].end <= pair[1].start, "{pair:?}");
    // White space, ASCII punctuation, a dash, an arrow and Thai text are boundaries.
    for text in [
        "10 15".to_owned(),
        "5+5".to_owned(),
        "5;5".to_owned(),
        "10\u{2014}15".to_owned(),
        "10\u{2192}15".to_owned(),
        format!("5{KHON}10{KHON}"),
    ] {
        let found = numbers(&text);
        assert_eq!(found.len(), 2, "{text:?}: {found:?}");
        assert!(
            found
                .iter()
                .all(|found| found.value.role == NumberRole::Quantity),
            "{text:?}: {found:?}"
        );
    }
}

/// Fix round 2: a code the lexer makes takes in its minus, accounting parentheses, sign word
/// and scale words, as a figure does, so a quote that drops them cuts it.
#[test]
fn codes_keep_their_sign_parentheses_and_scale() {
    for (text, code) in [
        (
            format!("{KHAT_THUN} \u{2212}1'250 {LAN}{BAHT}"),
            format!("\u{2212}1'250 {LAN}"),
        ),
        (
            format!("{KHAT_THUN} -1\u{2009}250 {LAN}{BAHT}"),
            format!("-1\u{2009}250 {LAN}"),
        ),
        (
            format!("{KHAT_THUN} (1\u{2009}250) {LAN}{BAHT}"),
            format!("(1\u{2009}250) {LAN}"),
        ),
        (
            format!("{KHAT_THUN} {TID_LOP} 1'250 {LAN}{BAHT}"),
            format!("{TID_LOP} 1'250 {LAN}"),
        ),
        ("SKU-12AB34".to_owned(), "SKU-12AB34".to_owned()),
    ] {
        let found = only(&text);
        assert_eq!(found.value.role, NumberRole::Code, "{text:?}");
        assert_eq!(span(&text, &found), code, "{text:?}");
    }
    // An ordinary word after a code is not its scale (mutant F-MASKSCALE-scaleend).
    let rank = format!("1'250 {PHANTRI}");
    assert_eq!(span(&rank, &only(&rank)), "1'250");
}

/// Fix round 2: digit groups with letters between them and nothing else are one code, so a
/// lookalike letter cannot leave a confident tail (`12O5 ล้านบาท` is no 5,000,000).
#[test]
fn digit_groups_with_letters_between_them_are_one_code() {
    for (text, code) in [
        (format!("{RAYDAI} 12O5 {LAN}{BAHT}"), format!("12O5 {LAN}")),
        (format!("{RAYDAI} 1l50 {LAN}{BAHT}"), format!("1l50 {LAN}")),
        (
            format!("รห\u{0E31}ส AB12CD34 {LAN}{BAHT}"),
            format!("AB12CD34 {LAN}"),
        ),
        ("A1B2".to_owned(), "A1B2".to_owned()),
        ("2Q68".to_owned(), "2Q68".to_owned()),
        ("1H25".to_owned(), "1H25".to_owned()),
        ("9M2025".to_owned(), "9M2025".to_owned()),
        ("1920x1080".to_owned(), "1920x1080".to_owned()),
        ("INV2025A001".to_owned(), "INV2025A001".to_owned()),
    ] {
        let found = only(&text);
        assert_eq!(found.value.role, NumberRole::Code, "{text:?}");
        assert_eq!(span(&text, &found), code, "{text:?}");
    }
    let model = format!("ร\u{0E38}\u{0E48}น A1B2 {RAKHA} 5 {LAN}{BAHT}");
    assert_eq!(confident(&model), vec![dec("5000000")]);
}

/// Fix round 2: a fraction before a scale word or a unit is one code: the reader has no grammar
/// for it, so its denominator never reads as the amount.
#[test]
fn fractions_before_a_scale_or_a_unit_are_one_code() {
    for (text, code) in [
        (format!("{BUDGET} 3/4 {LAN}{BAHT}"), format!("3/4 {LAN}")),
        (format!("{RAKHA} 1/2 {BAHT}"), "1/2".to_owned()),
        ("rate 1/2%".to_owned(), "1/2%".to_owned()),
        (
            format!("{BUDGET} 3\u{2044}4 {LAN}{BAHT}"),
            format!("3\u{2044}4 {LAN}"),
        ),
        (
            format!("{BUDGET} 3\u{2215}4 {LAN}{BAHT}"),
            format!("3\u{2215}4 {LAN}"),
        ),
        ("growth 5 1/2%".to_owned(), "5 1/2%".to_owned()),
        ("1 1/2 million baht".to_owned(), "1 1/2 million".to_owned()),
        ("THB 3/4 million".to_owned(), "3/4 million".to_owned()),
    ] {
        let found = only(&text);
        assert_eq!(found.value.role, NumberRole::Code, "{text:?}");
        assert_eq!(span(&text, &found), code, "{text:?}");
    }
    // Spaced, it is two figures joined by a slash: neither reads at 0.8.
    let spaced = format!("3 / 4 {LAN}{BAHT}");
    assert_eq!(quantities(&spaced).len(), 2);
    assert!(confident(&spaced).is_empty(), "{:?}", numbers(&spaced));
}

/// Fix round 2: a comma makes the figure after it a code only after a glyph the text layer could
/// not name or one lone letter (`x,250`); after a word or a Thai letter it is a list or a CSV
/// field.
#[test]
fn a_stray_comma_needs_a_lone_letter() {
    for text in [
        format!("{RAKHA} 100 {BAHT},250 {BAHT}"),
        format!("{CHAMNUAN} 3 {KHON},5 {KHON}"),
        "revenue,1250".to_owned(),
        format!("{RAYDAI}รวม,1250"),
        format!("{RAYDAI},1250"),
    ] {
        assert!(
            numbers(&text)
                .iter()
                .all(|found| found.value.role == NumberRole::Quantity),
            "{text:?}: {:?}",
            numbers(&text)
        );
    }
    for text in ["x,250", "l,250", "item A,250", "\u{FFFD},250"] {
        assert_eq!(only(text).value.role, NumberRole::Code, "{text:?}");
    }
}

/// Fix round 2: a comma group that is not three digits, before a scale word or a unit, is one
/// code; a list of figures with nothing after it keeps its figures.
#[test]
fn a_comma_group_that_is_not_three_digits_before_a_scale_is_a_code() {
    for (text, code) in [
        (format!("{RAKHA} 1,25 {LAN}{BAHT}"), format!("1,25 {LAN}")),
        (format!("1,5 {LAN}"), format!("1,5 {LAN}")),
        (format!("1234,567 {BAHT}"), "1234,567".to_owned()),
    ] {
        let found = only(&text);
        assert_eq!(found.value.role, NumberRole::Code, "{text:?}");
        assert_eq!(span(&text, &found), code, "{text:?}");
    }
    assert_eq!(values("1,2,3"), vec![dec("1"), dec("2"), dec("3")]);
    assert_eq!(values("1234,567"), vec![dec("1234"), dec("567")]);
}

/// Fix round 2: นาฬิกา, the word น. abbreviates, makes a time as น. does; hours stop at 24 at
/// the end of a range too (mutants I, M6b and N-CLOCK-24).
#[test]
fn the_thai_hour_word_makes_a_time() {
    for text in [
        format!("{WELA} 09.30 {NALIKA}"),
        format!("{WELA} 09.00 - 12.00 {NALIKA}"),
        format!("{WELA} 09.00 {THUENG} 12.00 {NALIKA}"),
        "24:00".to_owned(),
        format!("{WELA} 23.00 - 24.00 น."),
    ] {
        let found = numbers(&text);
        assert!(
            !found.is_empty()
                && found
                    .iter()
                    .all(|found| found.value.role == NumberRole::Time),
            "{text:?}: {found:?}"
        );
    }
    let late = numbers(&format!("{WELA} 09.00 - 25.00 น."));
    assert!(
        late.iter()
            .all(|found| found.value.role != NumberRole::Time),
        "{late:?}"
    );
}

/// Fix round 2: an amount written in parts split by one space (a line break included) joins
/// as one number when each part writes only places below the one before it, or is one code;
/// so is a figure with a scale followed by a figure written as an amount. No part is read alone.
#[test]
fn amounts_written_in_parts_join_or_become_one_code() {
    for (text, amount, written) in [
        (
            format!("{RAKHA} {SAM}{LAN} {HA}{SAEN}{BAHT}"),
            "3500000",
            format!("{SAM}{LAN} {HA}{SAEN}"),
        ),
        (
            format!("{RAKHA} {SAM}{LAN}\n{HA}{SAEN}{BAHT}"),
            "3500000",
            format!("{SAM}{LAN}\n{HA}{SAEN}"),
        ),
        (
            format!("{RAKHA} {NUENG}{LAN} 2 {SAEN}{BAHT}"),
            "1200000",
            format!("{NUENG}{LAN} 2 {SAEN}"),
        ),
        (
            format!("{SONG}{MUEN} {HA}{PHAN}{BAHT}"),
            "25000",
            format!("{SONG}{MUEN} {HA}{PHAN}"),
        ),
        (
            format!("{RAYDAI} 1 {LAN} 250,000 {BAHT}"),
            "1250000",
            format!("1 {LAN} 250,000"),
        ),
        (
            format!("3 {LAN} 9 {SAEN} 2,500 {BAHT}"),
            "3902500",
            format!("3 {LAN} 9 {SAEN} 2,500"),
        ),
        (
            format!("1 {LAN} 250 {BAHT}"),
            "1000250",
            format!("1 {LAN} 250"),
        ),
    ] {
        let found = only(&text);
        assert_eq!(found.value.effective(), dec(amount), "{text:?}");
        assert_eq!(found.value.unit, thb(), "{text:?}");
        assert_eq!(span(&text, &found), written, "{text:?}");
    }
    // The figure written as an amount sets the precision: to the baht, not to 50,000.
    assert_eq!(
        only(&format!("1 {LAN} 250,000 {BAHT}")).value.half_ulp(),
        dec("0.5")
    );
    for (text, code) in [
        (
            format!("{SAM}{LAN} {SAM}{LAN}{BAHT}"),
            format!("{SAM}{LAN} {SAM}{LAN}"),
        ),
        (
            format!("{NUENG}{PHAN} 2 {SAEN}{BAHT}"),
            format!("{NUENG}{PHAN} 2 {SAEN}"),
        ),
        (format!("1 {PHAN} 2,500 {BAHT}"), format!("1 {PHAN} 2,500")),
    ] {
        let found = only(&text);
        assert_eq!(found.value.role, NumberRole::Code, "{text:?}");
        assert_eq!(span(&text, &found), code, "{text:?}");
    }
    // A small bare figure still never joins: ซื้อ 3 ล้าน 5 ชิ้น is three million and five pieces.
    let pieces = format!("{SUE} 3 {LAN} 5 {CHIN}");
    assert_eq!(effective(&pieces), vec![dec("3000000"), dec("5")]);
    // In the amount-in-words parentheses, spaces inside them and between the parts.
    for words in [
        format!("({NUENG}{PHAN} {SONG}{ROI} {BAHT})"),
        format!("( {NUENG}{PHAN}{SONG}{ROI}{BAHT}{THUAN} )"),
        format!("({NUENG}{PHAN} {SONG}{ROI}{BAHT}{THUAN})"),
    ] {
        let text = format!("{KHA_CHAI} 1,200 {BAHT} {words}");
        let found = numbers(&text);
        assert_eq!(found.len(), 2, "{text:?}: {found:?}");
        assert_eq!(found[1].value.value, dec("1200"), "{text:?}");
        assert!(found[1].flags.contains(Flags::AMOUNT_IN_WORDS), "{text:?}");
    }
}

/// Fix round 2 (a regression of round 1): a currency word or symbol between two figures is the
/// first figure's, unless the second has no unit of its own and the first already has one, or it
/// is a symbol written against the second. Two currencies on one figure are not read as one.
#[test]
fn a_currency_between_two_figures_is_the_first_ones_unless_the_second_has_none() {
    let usd = || Some(Unit::Currency("USD"));
    let units = |text: &str| {
        numbers(text)
            .into_iter()
            .map(|found| found.value.unit)
            .collect::<Vec<_>>()
    };
    for (text, expected) in [
        (format!("{RAKHA} 100 USD 3,500 {BAHT}"), vec![usd(), thb()]),
        ("Price 100 USD 3,500 THB".to_owned(), vec![usd(), thb()]),
        (format!("{FEE} 10 USD 350 {BAHT}"), vec![usd(), thb()]),
        (format!("{RAKHA} 100 $ 3,500 {BAHT}"), vec![usd(), thb()]),
        (
            "100 USD 200 THB 300 USD".to_owned(),
            vec![usd(), thb(), usd()],
        ),
        (format!("5,000 USD 150 {BAHT}"), vec![usd(), thb()]),
        (
            "10 THB 5 percent 7 percent".to_owned(),
            vec![thb(), Some(Unit::Percent), Some(Unit::Percent)],
        ),
        ("1,250 THB\n35 USD".to_owned(), vec![thb(), usd()]),
        (
            "1,250 THB 35 USD 50 THB".to_owned(),
            vec![thb(), usd(), thb()],
        ),
        ("Fee 100 THB 35 USD".to_owned(), vec![thb(), usd()]),
        (
            "1,250 THB 12.5 percent".to_owned(),
            vec![thb(), Some(Unit::Percent)],
        ),
        (format!("$100 USD 3,500 {BAHT}"), vec![usd(), thb()]),
        ("USD 100 THB 3,500".to_owned(), vec![usd(), thb()]),
        (
            "THB 1,250 million USD 35 million".to_owned(),
            vec![thb(), usd()],
        ),
        ("5 $10".to_owned(), vec![None, usd()]),
        ("5\u{0E3F} 10".to_owned(), vec![thb(), None]),
        ("USD 100, THB 3,500".to_owned(), vec![usd(), thb()]),
        ("1 THB million".to_owned(), vec![thb()]),
    ] {
        assert_eq!(units(&text), expected, "{text:?}");
    }
    // The figure that keeps its currency stays a quantity, not an ordinal after ที่.
    let after_marker = format!("{YU_THI} 1,250 THB 2568");
    let found = numbers(&after_marker);
    assert_eq!(found[0].value.role, NumberRole::Quantity, "{found:?}");
    assert_eq!(found[0].value.unit, thb());
    assert_eq!(
        values("(1) USD 500 registration fee"),
        vec![dec("1"), dec("500")]
    );
    // The same currency written twice is one: `$100 USD` reads at 1.0.
    let dollars = format!("$100 USD 3,500 {BAHT}");
    assert!(numbers(&dollars).iter().all(|found| found.conf == 1.0));
    for text in ["USD 100 THB".to_owned(), format!("USD 100 {BAHT}")] {
        let found = only(&text);
        assert_eq!(found.value.unit, usd(), "{text:?}");
        assert!(found.conf < 0.8, "{text:?}");
        assert_eq!(found.alts.len(), 1, "{text:?}");
        assert_eq!(found.alts[0].0.unit, thb(), "{text:?}");
    }
}

/// Fix round 2: a percent or a currency symbol inside accounting parentheses may restate a rise
/// in prose, so it reads as the negative at most 0.6 with the positive beside it, unless it is
/// the whole text (a table cell); the plan's `(1,250)` and round 1's `(5) ล้านบาท` stay sure.
#[test]
fn a_parenthesised_percent_in_prose_is_no_sure_negative() {
    for text in [
        format!("{RAYDAI}{PHOEM} 100 {LAN}{BAHT} (5%)"),
        format!("{PHOEM} (5.2%) "),
        format!("{PANPHON} 0.75 {PER_SHARE} (5.0%)"),
        "Thailand (45%), Vietnam (30%)".to_owned(),
        "fee ($12) waived".to_owned(),
    ] {
        let found: Vec<_> = numbers(&text)
            .into_iter()
            .filter(|found| found.flags.contains(Flags::PAREN_NEGATIVE))
            .collect();
        assert!(!found.is_empty(), "{text:?}");
        for found in found {
            assert!(found.value.value.is_sign_negative(), "{text:?}");
            assert!(found.conf < 0.8, "{text:?}");
            assert_eq!(found.alts.len(), 1, "{text:?}");
            assert_eq!(found.alts[0].0.value, -found.value.value, "{text:?}");
        }
    }
    for (text, value) in [
        ("(5%)".to_owned(), "-5"),
        ("($12)".to_owned(), "-12"),
        (format!("{RAYDAI} (1,250)"), "-1250"),
        (format!("{KHAT_THUN} (5) {LAN}{BAHT}"), "-5"),
        ("revenue $(1,250)".to_owned(), "-1250"),
    ] {
        let found = only(&text);
        assert_eq!(found.value.value, dec(value), "{text:?}");
        assert_eq!(found.conf, 1.0, "{text:?}");
    }
    // The currency before the parentheses is the figure's (round-1 review, P3).
    assert_eq!(only("THB (1,250)").value.unit, thb());
    assert_eq!(only("$(1,250)").value.unit, Some(Unit::Currency("USD")));
}

/// Fix round 2: a power word followed by one digit word reads by the grammar at most 0.6, the
/// everyday reading one place lower beside it (หนึ่งหมื่นห้า is 10,005 by the grammar and 15,000
/// in speech); เอ็ด only ever means units, and a power word at the end leaves no doubt.
#[test]
fn a_digit_word_after_a_power_word_is_read_two_ways() {
    for (text, grammar, spoken) in [
        (format!("{SALARY}{NUENG}{MUEN}{HA}{BAHT}"), "10005", "15000"),
        (format!("{RAKHA}{SONG}{PHAN}{HA}{BAHT}"), "2005", "2500"),
        (format!("{RAKHA}{SAM}{ROI}{HA}{BAHT}"), "305", "350"),
        (format!("{RAKHA} 3 {LAN}{HA}{BAHT}"), "3000005", "3500000"),
        (format!("{RAKHA} 2 {MUEN}{HA}{BAHT}"), "20005", "25000"),
        (
            format!("{RAKHA} 1 {LAN}{SONG} {BAHT}"),
            "1000002",
            "1200000",
        ),
    ] {
        let found = only(&text);
        assert_eq!(found.value.effective(), dec(grammar), "{text:?}");
        assert!(found.conf < 0.8, "{text:?}");
        assert_eq!(found.alts.len(), 1, "{text:?}");
        assert_eq!(found.alts[0].0.effective(), dec(spoken), "{text:?}");
    }
    for (text, value) in [
        (format!("3 {LAN}{HA}{SAEN} {BAHT}"), "3500000"),
        (format!("{HA}{SIP}{ET}{LAN}{BAHT}"), "51000000"),
        (format!("{SONG}{ROI}{ET}{BAHT}"), "201"),
        (format!("{HA}{SIP}{HA} {BAHT}"), "55"),
    ] {
        let found = only(&text);
        assert_eq!(found.value.effective(), dec(value), "{text:?}");
        assert_eq!(found.conf, 1.0, "{text:?}");
    }
}

/// Fix round 2: in the amount-in-words parentheses, words that start with a power word restate
/// the figure when they equal it (สิบล้านบาทถ้วน); one that does not, and a bare unit header, stay
/// a code.
#[test]
fn power_led_words_that_restate_the_figure_are_amount_in_words() {
    for (figure, words) in [
        ("10,000,000", format!("{SIP}{LAN}{BAHT}{THUAN}")),
        ("15", format!("{SIP}{HA}{BAHT}{THUAN}")),
        ("100", format!("{ROI}{BAHT}")),
    ] {
        let text = format!("{figure} {BAHT} ({words})");
        let found = numbers(&text);
        assert_eq!(found.len(), 2, "{text:?}: {found:?}");
        assert!(found[1].flags.contains(Flags::AMOUNT_IN_WORDS), "{text:?}");
        assert_eq!(found[1].value.value, found[0].value.value, "{text:?}");
    }
    for text in [
        format!("1,000,000 {BAHT} ({SIP}{LAN}{BAHT}{THUAN})"),
        format!("1,250 ({LAN}{BAHT})"),
        format!("{LAI}{SIP}{LAN}{BAHT}"),
    ] {
        let words = numbers(&text);
        let last = words.last().unwrap();
        assert_eq!(last.value.role, NumberRole::Code, "{text:?}: {words:?}");
        assert!(!last.flags.contains(Flags::AMOUNT_IN_WORDS), "{text:?}");
    }
}

const KHA_PRAP: &str = "ค\u{0E48}\u{0E32}ปร\u{0E31}บ"; // ค่าปรับ

/// The founder's answer (round 2b): outside the amount-in-words parentheses, number words that
/// start with a power word stay one code of value 0, even with a unit after them, so a claim
/// about them is never checked.
#[test]
fn power_led_words_outside_the_parentheses_stay_one_code() {
    for (text, code) in [
        (format!("{KHA_PRAP}{SIP}{BAHT}"), SIP.to_owned()),
        (format!("{RAKHA}{SIP}{HA}{BAHT}"), format!("{SIP}{HA}")),
        (format!("{RAKHA} {ROI}{BAHT}"), ROI.to_owned()),
    ] {
        let found = only(&text);
        assert_eq!(found.value.role, NumberRole::Code, "{text:?}: {found:?}");
        assert_eq!(found.value.value, Decimal::ZERO, "{text:?}");
        assert_eq!(span(&text, &found), code, "{text:?}");
    }
}

/// Fix round 2: a Latin abbreviation of a scale one space after a figure makes it one code with
/// it (`$5 M`, `5 mln`); a lone lowercase letter is no scale and no code.
#[test]
fn latin_scale_abbreviations_make_a_code() {
    for (text, code) in [
        ("fee $5 M", "$5 M"),
        ("revenue THB 250 M", "250 M"),
        ("revenue THB 1.2 bln", "1.2 bln"),
        ("fee $5 mil", "$5 mil"),
        ("profit 5 mln baht", "5 mln"),
        ("Revenue 1,250 MB", "1,250 MB"),
        ("cost 7 K", "7 K"),
    ] {
        let found = only(text);
        assert_eq!(found.value.role, NumberRole::Code, "{text:?}");
        assert_eq!(span(text, &found), code, "{text:?}");
    }
    for text in ["2 m", "2 b", "5 mbps"] {
        assert_eq!(only(text).value.role, NumberRole::Quantity, "{text:?}");
    }
    // A letter glued to digits is no abbreviation, and a code never starts on a word a figure
    // before it took (round 2's random-text harness).
    for (text, code) in [("2568 M1q2", "M1q2"), ("1 million2b3", "2b3")] {
        let found = numbers(text);
        assert_eq!(found.len(), 2, "{text:?}: {found:?}");
        assert!(found[0].end <= found[1].start, "{text:?}: {found:?}");
        assert_eq!(found[1].raw, code, "{text:?}");
    }
}

/// Fix round 2 (round-1 review, P3): every code has value 0, scale 0 and no unit, however it was
/// made; a ลบ that ends a Thai word is no sign; `minus` right after a number is a subtraction or
/// a sign, so the figure is ambiguous.
#[test]
fn codes_read_no_value_and_sign_words_stand_alone() {
    for text in [
        "fee $5M",
        "COVID19",
        "x,250 baht",
        "revenue Bt1,250 million",
        "fee $5 M",
    ] {
        let found = only(text);
        assert_eq!(found.value.role, NumberRole::Code, "{text:?}");
        assert!(found.value.value.is_zero(), "{text:?}");
        assert_eq!(found.value.scale, Scale(0), "{text:?}");
        assert_eq!(found.value.unit, None, "{text:?}");
    }
    let fainted = only("สลบ 5");
    assert_eq!(fainted.value.value, dec("5"));
    assert_eq!(fainted.conf, 1.0);
    assert_eq!(span("สลบ 5", &fainted), "5");
    let subtraction = numbers("100 minus 30");
    assert_eq!(subtraction[1].value.value, dec("30"));
    assert_eq!(subtraction[1].conf, 0.5);
    assert_eq!(subtraction[1].alts[0].0.value, dec("-30"));
    assert_eq!(
        only("GDP contracted minus 2.5 percent").value.value,
        dec("-2.5")
    );
}

/// Fix round 2 (mutants F-REGROUPED-group, F-REGROUPED-wordgroup and F-MASKSCALE-amountafter):
/// the grammar-against-sum check holds for figure-led parts and for the later parts of a
/// compound, and an ordinary word is no scale after a list marker or a figure.
#[test]
fn regrouped_parts_and_ordinary_words_stay_unread() {
    for text in [
        format!("3 {PHAN}{LAN}{HA}{ROI}{LAN}{BAHT}"),
        format!("2 {MUEN}{LAN}{HA}{PHAN}{LAN}{BAHT}"),
        format!("1 {LAN}{LAN}{LAN} {SAM}{PHAN}{LAN}{HA}{ROI}{LAN}{BAHT}"),
        format!("{NUENG}{LAN}{LAN}{LAN} {SAM}{PHAN}{LAN}{HA}{ROI}{LAN}{BAHT}"),
        // The part before adds nothing, so only the later part's own check sees the regrouping.
        format!("0 {LAN}{LAN}{LAN} {SAM}{PHAN}{LAN}{HA}{ROI}{LAN}{BAHT}"),
    ] {
        assert_eq!(only(&text).value.role, NumberRole::Code, "{text:?}");
    }
    let marker = format!("(1) {PHANTRI}");
    let found = only(&marker);
    assert_eq!(found.value.value, dec("1"));
    assert!(!found.flags.contains(Flags::PAREN_NEGATIVE));
    let rank = format!("3 {LAN} {PHANTRI}");
    let found = only(&rank);
    assert_eq!(found.value.effective(), dec("3000000"));
    assert_eq!(span(&rank, &found), format!("3 {LAN}"));
}

/// Fix round 2: a sign word near a figure, with words between (ติดลบที่, ติดลบอยู่ที่ประมาณ),
/// leaves it positive at 0.6 with the negative beside it; right before it, it makes it negative.
#[test]
fn a_sign_word_near_a_figure_makes_it_ambiguous() {
    for text in [
        format!("{SETTHAKIT}{KHAYAI}{TID_LOP}{THI}{ROI_LA} 6.1"),
        format!("{SETTHAKIT}{TID_LOP}{THUENG}{ROI_LA} 6.1"),
        format!("{KAMRAI}{TID_LOP}{YU_THI} 5 {LAN}{BAHT}"),
        format!("{PHONPRAKOP}{TID_LOP} {CHAMNUAN} 5 {LAN}{BAHT}"),
        format!("{KAMRAI}{TID_LOP}{YU_THI}{PRAMAN} 5 {LAN}{BAHT}"),
        format!("{SETTHAKIT}{TID_LOP}{SUNG_THUENG}{ROI_LA} 6.1"),
        format!("{NGOEN_FOE}{TID_LOP}{KUEAP}{ROI_LA} 1"),
        format!("{PHONPRAKOP}{TID_LOP} {TAE}{RAYDAI} 5 {LAN}{BAHT}"),
        format!("{LOP}{THI} 5 {LAN}{BAHT}"),
    ] {
        let found = only(&text);
        assert!(!found.value.value.is_sign_negative(), "{text:?}");
        assert!(found.conf < 0.8, "{text:?}");
        assert!(
            found
                .alts
                .iter()
                .any(|(alt, _)| alt.value == -found.value.value),
            "{text:?}: {found:?}"
        );
    }
    let attached = format!("{SETTHAKIT}{TID_LOP}{ROI_LA} 6.1");
    let found = only(&attached);
    assert_eq!(found.value.value, dec("-6.1"));
    assert_eq!(found.conf, 1.0);
    // A sign word inside another figure's span is that figure's alone.
    let two = format!("{KAMRAI}{TID_LOP} 5% {RAYDAI} 10%");
    assert_eq!(confident(&two), vec![dec("-5"), dec("10")]);
    // "Near" is 15 code points of the normalized text, counted to the unit word before.
    for (gap, capped) in [(14, true), (15, false)] {
        let text = format!("{TID_LOP}{} 5 {LAN}{BAHT}", "ก".repeat(gap));
        let found = only(&text);
        assert_eq!(found.conf < 0.8, capped, "{gap}: {found:?}");
    }
}

/// Fix round 2: a range of figures caps both ends, and the bare end gets the scale and unit of
/// the other as its other reading (`10-15 ล้านบาท` may be 10,000,000 to 15,000,000); a list of
/// figures caps each item.
#[test]
fn ranges_and_lists_of_figures_are_not_read_with_confidence() {
    for (text, bare, carried) in [
        (format!("{RAYDAI} 10-15 {LAN}{BAHT}"), "10", "10000000"),
        (
            format!("{RAYDAI}{RAWANG} 10 {THUENG} 15 {LAN}{BAHT}"),
            "10",
            "10000000",
        ),
        (format!("{SALARY} 25,000 - 30,000 {BAHT}"), "25000", "25000"),
        ("growth 3-5%".to_owned(), "3", "3"),
        (format!("{ROI_LA} 3-5"), "5", "5"),
    ] {
        let found = quantities(&text);
        assert_eq!(found.len(), 2, "{text:?}: {found:?}");
        assert!(found.iter().all(|found| found.conf < 0.8), "{text:?}");
        let end = found
            .iter()
            .find(|found| found.value.value == dec(bare))
            .unwrap();
        assert!(
            end.alts
                .iter()
                .any(|(alt, _)| alt.effective() == dec(carried) && alt.unit.is_some()),
            "{text:?}: {end:?}"
        );
    }
    for text in [
        "total 3,451 and 200".to_owned(),
        format!("1,100 {LAN}{BAHT}, 1,250 {LAN}{BAHT} {LAE} 1,400 {LAN}{BAHT}"),
        format!("5 {LAE} 6 {LAN}{BAHT}"),
        "$5-6M".to_owned(),
        "10,20,30".to_owned(),
    ] {
        assert!(
            confident(&text).is_empty(),
            "{text:?}: {:?}",
            numbers(&text)
        );
    }
    // A list word between two figures with a subject between them joins nothing.
    let facts = format!("{RAYDAI} 100 {LAN}{BAHT} {LAE}{KAMRAI} 20 {LAN}{BAHT}");
    assert_eq!(confident(&facts), vec![dec("100000000"), dec("20000000")]);
    // Ends whose own scales or units differ lend each other nothing: a percent is no part of an
    // amount in millions (mutant C-AGREES).
    for text in [
        "revenue rose 5% to 1,250 million".to_owned(),
        format!("{KAMRAI} 270 {LAN}{BAHT} {HUE}{ROI_LA} 21.6"),
    ] {
        let found = quantities(&text);
        assert_eq!(found.len(), 2, "{text:?}: {found:?}");
        assert!(
            found
                .iter()
                .all(|found| found.conf < 0.8 && found.alts.is_empty()),
            "{text:?}: {found:?}"
        );
    }
}

const JONTHUENG: &str = "จนถ\u{0E36}ง"; // จนถึง
const PAI_JONTHUENG: &str = "\u{0E44}ปจนถ\u{0E36}ง"; // ไปจนถึง
const JONKRATHANG: &str = "จนกร\u{0E30}ท\u{0E31}\u{0E48}ง"; // จนกระทั่ง
const TANGTAE: &str = "ต\u{0E31}\u{0E49}ง\u{0E41}ต\u{0E48}"; // ตั้งแต่
const JAK: &str = "จ\u{0E32}ก"; // จาก
const NGUAT: &str = "งวด"; // งวด
const MONTHS_FIRST: &str = "\u{0E40}ด\u{0E37}อน\u{0E41}รก"; // เดือนแรก
const MONTHS_LAST: &str = "\u{0E40}ด\u{0E37}อนหล\u{0E31}ง"; // เดือนหลัง
const QUARTER: &str = "\u{0E44}ตรม\u{0E32}ส"; // ไตรมาส
const FISCAL_SHORT: &str = "ป\u{0E35}งบ"; // ปีงบ
const HUE: &str = "หร\u{0E37}อ"; // หรือ

/// Minutes in the demo's style: a meeting date, a fiscal year's revenue, expenses and profit,
/// a margin, an approved budget, a forecast written as a range, and a due date.
const MINUTES: &str = "บ\u{0E31}นท\u{0E36}กก\u{0E32}รปร\u{0E30}ช\u{0E38}มคณ\u{0E30}กรรมก\u{0E32}ร คร\u{0E31}\u{0E49}งท\u{0E35}\u{0E48} 5/2568 ว\u{0E31}นท\u{0E35}\u{0E48} 1 ต.ค. 2568 \u{0E40}วล\u{0E32} 13.30 น. ท\u{0E35}\u{0E48}ปร\u{0E30}ช\u{0E38}มร\u{0E31}บทร\u{0E32}บผลก\u{0E32}รด\u{0E33}\u{0E40}น\u{0E34}นง\u{0E32}นป\u{0E35}งบปร\u{0E30}ม\u{0E32}ณ 2568 ร\u{0E32}ย\u{0E44}ด\u{0E49}รวม 1,250 ล\u{0E49}\u{0E32}นบ\u{0E32}ท ค\u{0E48}\u{0E32}\u{0E43}ช\u{0E49}จ\u{0E48}\u{0E32}ยรวม 980 ล\u{0E49}\u{0E32}นบ\u{0E32}ท \u{0E41}ล\u{0E30}ก\u{0E33}\u{0E44}รส\u{0E38}ทธ\u{0E34} 270 ล\u{0E49}\u{0E32}นบ\u{0E32}ท อ\u{0E31}ตร\u{0E32}ก\u{0E33}\u{0E44}รร\u{0E49}อยล\u{0E30} 21.6 ท\u{0E35}\u{0E48}ปร\u{0E30}ช\u{0E38}มม\u{0E35}มต\u{0E34}อน\u{0E38}ม\u{0E31}ต\u{0E34}งบลงท\u{0E38}นป\u{0E35}งบปร\u{0E30}ม\u{0E32}ณ 2569 จ\u{0E33}นวน 410 ล\u{0E49}\u{0E32}นบ\u{0E32}ท \u{0E41}ล\u{0E30}ค\u{0E32}ดว\u{0E48}\u{0E32}ร\u{0E32}ย\u{0E44}ด\u{0E49}ป\u{0E35}งบปร\u{0E30}ม\u{0E32}ณ 2569 จ\u{0E30}\u{0E40}ต\u{0E34}บ\u{0E42}ตร\u{0E49}อยล\u{0E30} 5-7 \u{0E43}ห\u{0E49}ฝ\u{0E48}\u{0E32}ยบ\u{0E31}ญช\u{0E35}ส\u{0E48}งร\u{0E32}ยง\u{0E32}นภ\u{0E32}ย\u{0E43}นว\u{0E31}นท\u{0E35}\u{0E48} 15 ต.ค. 2568";

/// Fix round 2 (the opposite failure): in minutes like the demo's, every plain figure and date
/// keeps its confidence; only the forecast range drops below 0.8, with its other reading.
#[test]
fn demo_minutes_keep_their_plain_figures_confident() {
    let found = numbers(MINUTES);
    let sure: Vec<(String, Decimal)> = found
        .iter()
        .filter(|found| found.value.role == NumberRole::Quantity && found.conf >= 0.8)
        .map(|found| (found.raw.clone(), found.value.effective()))
        .collect();
    assert_eq!(
        sure,
        [
            (format!("1,250 {LAN}"), dec("1250000000")),
            (format!("980 {LAN}"), dec("980000000")),
            (format!("270 {LAN}"), dec("270000000")),
            ("21.6".to_owned(), dec("21.6")),
            (format!("410 {LAN}"), dec("410000000")),
        ]
    );
    let range: Vec<_> = found
        .iter()
        .filter(|found| found.value.role == NumberRole::Quantity && found.conf < 0.8)
        .collect();
    assert_eq!(
        range
            .iter()
            .map(|found| found.raw.as_str())
            .collect::<Vec<_>>(),
        ["5", "7"]
    );
    assert_eq!(range[1].alts[0].0.unit, Some(Unit::Percent));
    let dates = find_dates(MINUTES, &ctx()).unwrap();
    assert_eq!(
        dates
            .iter()
            .map(|found| (found.raw.as_str(), found.conf))
            .collect::<Vec<_>>(),
        [
            ("1 ต.ค. 2568", 0.9),
            ("ป\u{0E35}งบปร\u{0E30}ม\u{0E32}ณ 2568", 0.85),
            ("ป\u{0E35}งบปร\u{0E30}ม\u{0E32}ณ 2569", 0.85),
            ("ป\u{0E35}งบปร\u{0E30}ม\u{0E32}ณ 2569", 0.85),
            ("15 ต.ค. 2568", 0.9),
        ]
    );
}

/// Fix round 2: every signal the founder named, placed where it counts, keeps the reading near
/// it below 0.8 (a figure for ranges, lists, fractions and signs; a year for periods).
#[test]
fn every_signal_the_founder_named_caps_a_reading() {
    let figure = |text: &str| {
        let found = quantities(text);
        assert!(!found.is_empty(), "{text:?}: {:?}", numbers(text));
        found.iter().all(|found| found.conf < 0.8)
    };
    let date = |text: &str| {
        let found = find_dates(text, &ctx()).unwrap();
        !found.is_empty() && found.iter().all(|found| found.conf < 0.8)
    };
    for text in [
        format!("{RAYDAI}{THUENG} 1,250 {LAN}{BAHT}"),
        format!("{RAYDAI}{JONTHUENG} 1,250 {LAN}{BAHT}"),
        format!("{RAYDAI}{PAI_JONTHUENG} 1,250 {LAN}{BAHT}"),
        format!("{RAYDAI}{JONKRATHANG} 1,250 {LAN}{BAHT}"),
        format!("{RAYDAI}{RAWANG} 1,250 {LAN}{BAHT} {KAP}{PI}{KORN}"),
        format!("{RAYDAI}{TANGTAE} 1,250 {LAN}{BAHT}"),
        format!("{RAYDAI}{JAK} 1,250 {LAN}{BAHT}"),
        format!("{KAMRAI}{TID_LOP}{YU_THI} 5%"),
        format!("{LOP} {YU_THI} 5%"),
        format!("1,250 {LAN}{BAHT} {LAE} 2,500 {LAN}{BAHT}"),
        format!("1,250 {LAN}{BAHT} {HUE} 2,500 {LAN}{BAHT}"),
        "between 1,250 and the target".to_owned(),
        "from 1,250".to_owned(),
        "to 1,250".to_owned(),
        "through 1,250".to_owned(),
        "thru 1,250".to_owned(),
        "till 1,250".to_owned(),
        "until 1,250".to_owned(),
        "up to 1,250".to_owned(),
        "1,250 and 2,500".to_owned(),
        "1,250 or 2,500".to_owned(),
        "1,250-2,500".to_owned(),
        "1,250 / 2,500".to_owned(),
        "1,250 -> 2,500".to_owned(),
        "1,250 \u{2192} 2,500".to_owned(),
        "1,250, 2,500".to_owned(),
    ] {
        assert!(figure(&text), "{text:?}: {:?}", numbers(&text));
    }
    for text in [
        format!("{TANGTAE} 31 ต.ค. 2568"),
        format!("{JONKRATHANG} 31 ต.ค. 2568"),
        format!("{THUENG}{WAN}{THI} 31 ต.ค. 2568"),
        format!("{NGUAT} {PI} 2568"),
        format!("{KHRUENG}{PI}\u{0E41}รก {PI} 2568"),
        format!("6 {MONTHS_FIRST} {PI} 2568"),
        format!("6 {MONTHS_LAST} {PI} 2568"),
        format!("{QUARTER} {PI} 2568"),
        format!("{FISCAL_SHORT} {PI} 2568"),
        "H1 FY2026".to_owned(),
        "FY2026 H2".to_owned(),
        "FY2026 Q1".to_owned(),
        "FY2026 Q2".to_owned(),
        "FY2026 Q3".to_owned(),
        "FY2026 Q4".to_owned(),
        "1 Oct 2025 - 31 Dec 2025".to_owned(),
        format!("{PI} 2567 / {PI} 2568"),
        "1 Oct 2025, 2 Oct 2025".to_owned(),
    ] {
        assert!(date(&text), "{text:?}: {:?}", find_dates(&text, &ctx()));
    }
    // A plain neighbour keeps both confident.
    assert_eq!(
        confident(&format!("{RAYDAI} 1,250 {LAN}{BAHT}")),
        vec![dec("1250000000")]
    );
    assert_eq!(only_date(&format!("{PI} 2568")).conf, 0.9);
}

const KAP: &str = "ก\u{0E31}บ"; // กับ
const KORN: &str = "ก\u{0E48}อน"; // ก่อน
const BORISAT: &str = "บร\u{0E34}ษ\u{0E31}ท"; // บริษัท

/// The founder's answer (round 2b): ระหว่าง and `between` cap a reading only when a range or
/// list connector follows it before the next clause; alone they mean "during" or "among", and
/// the reading stays plain. A connector with no reading after it caps nothing by itself.
#[test]
fn between_caps_a_reading_only_with_a_connector_after_it() {
    let capped = |text: &str| {
        let numbers = quantities(text);
        let dates = find_dates(text, &ctx()).unwrap();
        assert!(!(numbers.is_empty() && dates.is_empty()), "{text:?}");
        numbers.iter().all(|found| found.conf <= 0.6) && dates.iter().all(|found| found.conf <= 0.6)
    };
    for text in [
        format!("{RAWANG}{PI} 2567 {THUENG} 2568"),
        format!("{RAWANG}{PI} 2567 {LAE} 2568"),
        "between 2024 and 2025".to_owned(),
        format!("{RAYDAI}{RAWANG} 1,250 {LAN}{BAHT} {KAP}{PI}{KORN}"),
        "between 1,250 and the target".to_owned(),
    ] {
        assert!(capped(&text), "{text:?}: {:?}", numbers(&text));
    }
    // Alone, or with the connector in the next clause, they cap nothing.
    for text in [
        format!("{RAWANG}{PI} 2568"),
        format!("{RAWANG}{PI} 2568 {RAYDAI}{LAE}{KAMRAI}{BORISAT}"),
    ] {
        assert_eq!(only_date(&text).conf, 0.9, "{text:?}");
    }
    assert_eq!(confident("during 2568"), vec![dec("2568")]);
    assert_eq!(confident(&format!("between 3 {BORISAT}")), vec![dec("3")]);
    assert_eq!(
        confident(&format!("{RAYDAI} 1,250 {LAN}{BAHT} {KAP}{PI}{KORN}")),
        vec![dec("1250000000")]
    );
}

fn only_date(text: &str) -> Found<katwork_thai::DateValue> {
    let mut found = find_dates(text, &ctx()).unwrap();
    assert_eq!(found.len(), 1, "{text:?}: {found:?}");
    found.remove(0)
}
