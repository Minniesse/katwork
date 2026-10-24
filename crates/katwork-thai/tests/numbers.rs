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
    assert_eq!(code[0].value.value, dec("401"));
    assert_eq!(code[1].value.value, dec("123"));
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
    // Number words followed by a unit are read; without one they are not.
    assert_eq!(only(&format!("{SAM}{ROI}{BAHT}")).value.value, dec("300"));
    assert!(numbers(&format!("{SAM}{ROI}{KHON}")).is_empty());
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
    assert!(numbers(&LAN.repeat(40_000)).is_empty());
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
