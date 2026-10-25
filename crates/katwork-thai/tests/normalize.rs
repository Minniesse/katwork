//! `normalize_th`, `match_key`, the offset map and the PyThaiNLP port.

use katwork_thai::{NORMALIZE_VERSION, match_key, normalize_pass, normalize_th, pythainlp_compat};
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

fn fixture(name: &str) -> Vec<(String, Option<String>)> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    json["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| {
            let input = case["input"].as_str().unwrap().to_owned();
            (input, case["expected"].as_str().map(str::to_owned))
        })
        .collect()
}

/// SplitMix64: a seeded generator, so every random input is the same on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[usize::try_from(self.next() % u64::try_from(items.len()).unwrap()).unwrap()]
    }

    fn string(&mut self, alphabet: &[char], max_len: u64) -> String {
        let len = self.next() % (max_len + 1);
        (0..len).map(|_| *self.pick(alphabet)).collect()
    }
}

/// Thai consonants, vowels, every Thai mark, digits, spaces, the invisible characters step 1
/// removes, and the compatibility forms step 3 maps (and some it leaves alone).
const THAI_SOUP: &[char] = &[
    'ก', 'ข', 'น', 'ท', 'ฤ', 'ฦ', 'ร', 'อ', '\u{0E30}', '\u{0E31}', '\u{0E32}', '\u{0E33}',
    '\u{0E34}', '\u{0E35}', '\u{0E36}', '\u{0E37}', '\u{0E38}', '\u{0E39}', '\u{0E3A}', '\u{0E40}',
    '\u{0E41}', '\u{0E44}', '\u{0E45}', '\u{0E47}', '\u{0E48}', '\u{0E49}', '\u{0E4A}', '\u{0E4B}',
    '\u{0E4C}', '\u{0E4D}', '\u{0E4E}', '๑', '5', ' ', ' ', '\t', '\u{00A0}', '\u{200B}',
    '\u{200D}', '\u{00AD}', 'A', 'b', '\u{FB01}', '\u{FF04}', '\u{FF05}', '\u{FF08}', '\u{FF09}',
    '\u{FF0D}', '\u{2010}', '\u{2018}', '\u{2019}', '\u{2013}', '\u{2063}', '\u{001C}', '\u{00B2}',
];

fn grapheme_bounds(text: &str) -> Vec<u32> {
    let mut bounds: Vec<u32> = text
        .grapheme_indices(true)
        .map(|(at, _)| u32::try_from(text[..at].chars().count()).unwrap())
        .collect();
    bounds.push(u32::try_from(text.chars().count()).unwrap());
    bounds
}

fn len(text: &str) -> usize {
    text.chars().count()
}

#[test]
fn normalize_matches_pythainlp_oracle() {
    let cases = fixture("normalize.json");
    assert!(
        cases.len() >= 200,
        "normalize.json has only {} cases",
        cases.len()
    );
    let inputs: Vec<&str> = cases.iter().map(|(input, _)| input.as_str()).collect();
    let has = |what: &str, test: &dyn Fn(&str) -> bool| {
        assert!(inputs.iter().any(|i| test(i)), "no case with {what}")
    };
    has("a repeated lakkhangyao", &|i| {
        i.contains("\u{0E45}\u{0E45}")
    });
    has("three sara e", &|i| i.contains(&"\u{0E40}".repeat(3)));
    has("four sara e", &|i| i.contains(&"\u{0E40}".repeat(4)));
    has("two tone marks in a row", &|i| {
        i.contains("\u{0E48}\u{0E49}")
    });
    has("a space between repeated vowels", &|i| {
        i.contains("\u{0E35} \u{0E35}")
    });
    has("a mark right after a space", &|i| i.contains(" \u{0E48}"));
    has("a newline run", &|i| i.contains("\n\n"));
    has("U+3000 at both ends", &|i| {
        i.starts_with('\u{3000}') && i.ends_with('\u{3000}')
    });
    has("ZWSP", &|i| i.contains('\u{200B}'));
    has("mixed Thai and Latin", &|i| i.contains("Total"));
    let failures: Vec<_> = cases
        .iter()
        .filter_map(|(input, expected)| {
            let got = pythainlp_compat(input);
            (Some(&got) != expected.as_ref())
                .then(|| format!("{input:?}: got {got:?}, oracle {expected:?}"))
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} cases differ:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

#[test]
fn tone_order_variants_share_a_match_key() {
    assert_eq!(
        match_key("\u{0E19}\u{0E49}\u{0E33}").text,
        match_key("\u{0E19}\u{0E33}\u{0E49}").text
    );
    assert_eq!(
        match_key("\u{0E17}\u{0E35}\u{0E48}").text,
        match_key("\u{0E17}\u{0E48}\u{0E35}").text
    );
}

#[test]
fn sara_am_and_tone_orders_share_a_match_key() {
    for tone in ['\u{0E48}', '\u{0E49}', '\u{0E4A}', '\u{0E4B}'] {
        let keys: Vec<String> = [
            format!("น{tone}\u{0E4D}\u{0E32}"),
            format!("น\u{0E4D}{tone}\u{0E32}"),
            format!("น\u{0E33}{tone}"),
            format!("น{tone}\u{0E33}"),
        ]
        .iter()
        .map(|form| match_key(form).text)
        .collect();
        assert!(
            keys.iter().all(|key| *key == format!("น{tone}\u{0E33}")),
            "{keys:?}"
        );
    }
}

#[test]
fn sara_am_decomposed_equals_composed() {
    assert_eq!(
        match_key("\u{0E19}\u{0E49}\u{0E4D}\u{0E32}").text,
        match_key("\u{0E19}\u{0E49}\u{0E33}").text
    );
    assert!(normalize_th("\u{0E17}\u{0E33}").text.contains('\u{0E33}'));
}

#[test]
fn zero_width_and_soft_hyphen_removed() {
    let plain = "\u{0E23}\u{0E32}\u{0E22}\u{0E44}\u{0E14}\u{0E49}\u{0E23}\u{0E27}\u{0E21}";
    let (head, tail) = plain.split_at(plain.char_indices().nth(6).unwrap().0);
    for invisible in [
        '\u{200B}', '\u{00AD}', '\u{FEFF}', '\u{200C}', '\u{200D}', '\u{2060}',
    ] {
        assert_eq!(
            match_key(&format!("{head}{invisible}{tail}")).text,
            match_key(plain).text,
            "{invisible:?}"
        );
    }
}

#[test]
fn digits_fold_in_both_directions() {
    assert_eq!(match_key("๒๕๖๘").text, match_key("2568").text);
    assert_eq!(match_key("2568").text, "2568");
    assert_eq!(match_key("\u{0662}\u{0665}\u{0666}\u{0668}").text, "2568");
    assert_eq!(match_key("\u{06F2}\u{06F5}\u{06F6}\u{06F8}").text, "2568");
    assert_eq!(match_key("\u{FF12}\u{FF15}\u{FF16}\u{FF18}").text, "2568");
}

#[test]
fn whitespace_collapses_and_latin_lowercases() {
    assert_eq!(
        normalize_th("  Total\u{00A0}\u{00A0}Revenue ").text,
        "total revenue"
    );
}

#[test]
fn normalize_th_collapses_every_white_space_character() {
    let white_space: Vec<char> = (0..=0x3000)
        .filter_map(char::from_u32)
        .filter(|c| c.is_whitespace())
        .collect();
    assert_eq!(white_space.len(), 25);
    for c in white_space {
        assert_eq!(
            normalize_th(&format!("{c}ก{c}{c}ข{c}")).text,
            "ก ข",
            "{c:?}"
        );
    }
}

#[test]
fn offsets_round_trip_to_original() {
    let mut rng = Rng(20_261_025);
    for _ in 0..1_000 {
        let input = rng.string(THAI_SOUP, 24);
        let normalized = normalize_th(&input);
        let input_len = u32::try_from(len(&input)).unwrap();
        let text_len = len(&normalized.text);
        assert_eq!(
            normalized.original_range(0, text_len),
            0..input_len,
            "{input:?}"
        );
        let bounds = grapheme_bounds(&input);
        for start in 0..=text_len {
            for end in start..=text_len {
                let range = normalized.original_range(start, end);
                assert!(
                    range.start <= range.end && range.end <= input_len,
                    "{input:?} {start}..{end}: {range:?}"
                );
                assert!(
                    bounds.contains(&range.start) && bounds.contains(&range.end),
                    "{input:?} {start}..{end}: {range:?}"
                );
            }
        }
    }
}

#[test]
fn original_range_covers_deleted_edges() {
    let input = "\u{200B}\u{0E19}\u{0E49}\u{0E33}\u{200B}";
    let normalized = normalize_th(input);
    assert_eq!(normalized.text, "\u{0E19}\u{0E49}\u{0E33}");
    assert_eq!(normalized.original_range(0, 3), 0..5);
    // The leading ZWSP joins the first kept code point, the trailing one the last.
    assert_eq!(normalized.original_range(0, 1).start, 0);
    assert_eq!(normalized.original_range(2, 3).end, 5);
    // A space match_key removes joins the code point after it.
    let key = match_key("\u{0E01} \u{0E02}");
    assert_eq!(key.text, "\u{0E01}\u{0E02}");
    assert_eq!(key.original_range(1, 2), 1..3);
    assert_eq!(key.original_range(0, 2), 0..3);
}

#[test]
fn original_range_of_an_empty_text_is_the_whole_input() {
    let normalized = normalize_th("\u{200B} \u{0E48}");
    assert_eq!(normalized.text, "");
    assert_eq!(normalized.original_range(0, 0), 0..3);
}

#[test]
fn original_range_never_panics_on_odd_arguments() {
    let input = "\u{0E01}\u{0E48}\u{0E32} total  \u{200B}\u{0E19}";
    let input_len = u32::try_from(len(input)).unwrap();
    let normalized = normalize_th(input);
    let text_len = len(&normalized.text);
    for (start, end) in [
        (2, 2),
        (3, 1),
        (0, text_len + 50),
        (text_len, text_len),
        (text_len + 3, text_len + 9),
        (usize::MAX, 0),
        (0, usize::MAX),
        (usize::MAX, usize::MAX),
    ] {
        let range = normalized.original_range(start, end);
        assert!(
            range.start <= range.end && range.end <= input_len,
            "{start}..{end}: {range:?}"
        );
        if start >= end.min(text_len) {
            assert!(range.is_empty(), "{start}..{end}: {range:?}");
        }
    }
    assert_eq!(normalized.original_range(0, usize::MAX), 0..input_len);
}

#[test]
fn normalize_is_idempotent() {
    let mut inputs: Vec<String> = ["normalize.json", "numbers.json", "dates.json"]
        .iter()
        .flat_map(|name| fixture(name).into_iter().map(|(input, _)| input))
        .collect();
    let mut rng = Rng(2_569);
    inputs.extend((0..10_000).map(|_| rng.string(THAI_SOUP, 16)));
    let mut unstable = 0;
    for input in &inputs {
        let once = normalize_th(input);
        if !once.stable() {
            unstable += 1;
            continue;
        }
        let twice = normalize_th(&once.text);
        assert_eq!(twice.text, once.text, "{input:?}");
        assert!(twice.stable());
        assert_eq!(
            match_key(&once.text).text,
            match_key(input).text,
            "{input:?}"
        );
    }
    assert!(
        unstable * 100 <= inputs.len(),
        "{unstable} of {} inputs hit the pass cap",
        inputs.len()
    );
}

#[test]
fn normalize_is_idempotent_for_all_three_mark_combinations() {
    let marks: Vec<char> = "\u{0E31}\u{0E32}\u{0E33}\u{0E34}\u{0E35}\u{0E36}\u{0E37}\u{0E38}\u{0E39}\u{0E3A}\u{0E40}\u{0E45}\u{0E47}\u{0E48}\u{0E49}\u{0E4A}\u{0E4B}\u{0E4C}\u{0E4D}\u{0E4E} \u{200B}"
        .chars()
        .collect();
    for a in &marks {
        for b in &marks {
            for c in &marks {
                let input: String = ['ก', *a, *b, *c].iter().collect();
                let once = normalize_th(&input);
                assert!(once.stable(), "{input:?}");
                assert_eq!(normalize_th(&once.text).text, once.text, "{input:?}");
            }
        }
    }
}

#[test]
fn pass_cap_is_reported_and_never_exceeded() {
    // Each pass moves only the first tone mark of the chain behind its vowel, so twelve pairs need twelve passes.
    let input = format!("ก{}", "\u{0E48}\u{0E31}".repeat(12));
    let mut passes = vec![input.clone()];
    for _ in 0..9 {
        let next = normalize_pass(passes.last().unwrap());
        assert_ne!(
            &next,
            passes.last().unwrap(),
            "the chain converged after {} passes",
            passes.len() - 1
        );
        passes.push(next);
    }
    let normalized = normalize_th(&input);
    assert!(!normalized.stable());
    assert_eq!(normalized.text, passes[8]);
    assert_ne!(normalized.text, passes[9]);
    // Inputs that converge within the cap are stable.
    let short = format!("ก{}", "\u{0E48}\u{0E31}".repeat(3));
    assert!(normalize_th(&short).stable());
    assert_eq!(normalize_th(&short).text, "ก\u{0E31}\u{0E48}");
}

#[test]
fn compat_is_pythainlp_not_normalize_th() {
    let input = "\u{3000}\u{001C}ก\u{200D}ข\u{2060}ค\u{FEFF}ง\u{00AD}จ\tฉ\u{00A0}ช\u{001C}\u{3000}";
    assert_eq!(
        pythainlp_compat(input),
        "ก\u{200D}ข\u{2060}ค\u{FEFF}ง\u{00AD}จ\tฉ\u{00A0}ช"
    );
    // normalize_th removes the invisible and control characters (U+001C among them) and folds
    // every White_Space run to one space.
    assert_eq!(normalize_th(input).text, "กขคงจ ฉ ช");
    // PyThaiNLP removes a space before a mark only after a consonant no vowel precedes; normalize_th always does.
    let spaced = "\u{0E40}ก \u{0E48}";
    assert_eq!(pythainlp_compat(spaced), "\u{0E40}ก ");
    assert_eq!(normalize_th(spaced).text, "\u{0E40}ก\u{0E48}");
}

#[test]
fn latin_folds_and_lowercasing() {
    assert_eq!(
        match_key("\u{FB01}nancial").text,
        match_key("financial").text
    );
    assert_eq!(match_key("didn\u{2019}t").text, match_key("didn't").text);
    assert_ne!(match_key("m\u{00B2}").text, match_key("m2").text);
    assert_ne!(match_key("10\u{00B2}").text, match_key("102").text);
    assert_eq!(match_key("\u{2212}5 \u{2013} 7\u{2012}8").text, "-5-7-8");
    assert_eq!(match_key("\u{FF21}\u{FF42}\u{FF43}").text, "abc");
    let dotted = normalize_th("\u{0130}");
    assert_eq!(dotted.text, "\u{0130}");
    assert_eq!(dotted.original_range(0, 1), 0..1);
    assert_eq!(
        normalize_th("\u{0130}STANBUL \u{00C9}t\u{00C9}").text,
        "\u{0130}stanbul \u{00E9}t\u{00E9}"
    );
    assert_eq!(normalize_th("\u{0391}\u{0411}").text, "\u{0391}\u{0411}");
}

#[test]
fn sara_am_recomposed_from_two_code_points_widens_to_input_clusters() {
    let normalized = normalize_th("\u{0E17}\u{0E4D}\u{0E32}");
    assert_eq!(normalized.text, "\u{0E17}\u{0E33}");
    // The input is two clusters, ทํ and า; SARA AM came from both, so it maps to both.
    assert_eq!(normalized.original_range(1, 2), 0..3);
    assert_eq!(normalized.original_range(0, 1), 0..2);
}

#[test]
fn chunked_nfc_equals_whole_string_nfc() {
    let alphabet: Vec<char> = "aeiouyz\u{00E9}\u{1EC7}\u{0300}\u{0301}\u{0302}\u{0308}\u{0323}\u{031B}\u{0344}\u{0345}\u{1100}\u{1161}\u{11A8}\u{AC00}\u{0B47}\u{0B3E}\u{0B57}\u{0958}\u{093C}\u{0915}\u{3099}\u{304B}"
        .chars()
        .collect();
    let mut rng = Rng(15);
    for _ in 0..20_000 {
        let input = rng.string(&alphabet, 10);
        assert_eq!(
            normalize_pass(&input),
            input.nfc().collect::<String>(),
            "{input:?}"
        );
    }
}

#[test]
fn fixtures_escape_marks_and_invisibles() {
    for name in ["normalize.json", "numbers.json", "dates.json"] {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let raw = std::fs::read_to_string(path).unwrap();
        let space_like = |c: char| {
            matches!(
                c,
                '\u{00A0}' | '\u{1680}' | '\u{2000}'
                    ..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
            )
        };
        let bad: Vec<_> = raw
            .chars()
            .filter(|&c| {
                matches!(c, '\u{0E31}'..='\u{0E4E}' | '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}' | '\u{00AD}')
                    || space_like(c)
            })
            .collect();
        assert!(bad.is_empty(), "{name} holds raw {bad:?}");
    }
}

#[test]
fn nfkc_is_never_applied() {
    let text = "\u{0E17}\u{0E33}";
    assert_ne!(normalize_th(text).text, text.nfkc().collect::<String>());
    assert_eq!(normalize_th(text).text, text);
    // Step 3 recomposes SARA AM, so the forms only NFKC would fold are what show it was applied.
    for kept in [
        "m\u{00B2}",
        // A superscript with a combining mark is a chunk of two code points.
        "m\u{00B2}\u{0323}",
        "\u{2460}",
        "\u{2122}",
        "\u{00BD}",
        "\u{338F}",
        "\u{FF76}",
        "\u{FB05}",
    ] {
        assert_eq!(normalize_th(kept).text, kept, "{kept:?}");
        assert_ne!(
            normalize_th(kept).text,
            kept.nfkc().collect::<String>(),
            "{kept:?}"
        );
    }
}

#[test]
fn hostile_runs_stay_linear_and_bounded() {
    let tones = format!("ก{}", "\u{0E48}".repeat(199_999));
    assert_eq!(normalize_th(&tones).text, "ก\u{0E48}");
    let chain = format!("ก{}", "\u{0E48}\u{0E31}".repeat(99_999));
    let normalized = normalize_th(&chain);
    assert!(!normalized.stable());
    assert_eq!(
        normalized.original_range(0, len(&normalized.text)),
        0..200_000 - 1
    );
    let invisible = "\u{200B}".repeat(200_000);
    assert_eq!(normalize_th(&invisible).text, "");
    let sara_e = "\u{0E40}".repeat(200_000);
    assert_eq!(normalize_th(&sara_e).text, "\u{0E41}");
    let spaces = format!("ก{}ข", " \u{00A0}\t".repeat(66_666));
    assert_eq!(normalize_th(&spaces).text, "ก ข");
}

/// FNV-1a over every output, so a change to the rules shows up here before it reaches stored spans.
#[test]
fn normalize_output_is_frozen_for_version_1() {
    assert_eq!(NORMALIZE_VERSION, 1);
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |text: &str| {
        for byte in text.bytes().chain([0xff]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    };
    // Seeded inputs only, so editing a fixture file never moves the fingerprint.
    let mut rng = Rng(1);
    for _ in 0..5_000 {
        let input = rng.string(THAI_SOUP, 16);
        feed(&normalize_th(&input).text);
        feed(&match_key(&input).text);
    }
    assert_eq!(
        hash, FROZEN_V1,
        "normalize_th or match_key output changed (fingerprint {hash:#018x}). Spans store match_key with \
         normalize_version, so a change needs NORMALIZE_VERSION bumped and this fingerprint updated with it."
    );
}

/// The fingerprint of version 1, recorded when the rules were written.
const FROZEN_V1: u64 = 0x068f_43bd_1713_10ab;

/// Fix round 1: every Default_Ignorable code point and every control that is not White_Space
/// goes in step 1, so none can split a figure or a word.
#[test]
fn invisible_and_control_characters_are_removed() {
    let invisible = [
        '\u{2061}',
        '\u{2062}',
        '\u{2063}',
        '\u{2064}',
        '\u{034F}',
        '\u{FE00}',
        '\u{FE0F}',
        '\u{E0100}',
        '\u{180B}',
        '\u{180E}',
        '\u{17B4}',
        '\u{206A}',
        '\u{1D173}',
        '\u{200E}',
        '\u{200F}',
        '\u{061C}',
        '\u{3164}',
        '\u{FFF9}',
        '\u{0001}',
        '\u{001C}',
        '\u{007F}',
        '\u{0080}',
        '\u{202A}',
        '\u{2066}',
        '\u{E0041}',
        '\u{115F}',
        '\u{FFA0}',
    ];
    for c in invisible {
        assert_eq!(match_key(&format!("1{c}250")).text, "1250", "{c:?}");
        assert_eq!(normalize_th(&format!("ก{c}ข")).text, "กข", "{c:?}");
    }
    // Controls that are White_Space still become spaces.
    assert_eq!(normalize_th("ก\tข\u{000B}ค\u{0085}ง").text, "ก ข ค ง");
}

/// Fix round 1: the hyphens and minus signs that look like a hyphen-minus fold to it.
#[test]
fn dash_like_minus_signs_fold_to_hyphen_minus() {
    for dash in [
        '\u{2010}', '\u{2011}', '\u{FE63}', '\u{02D7}', '\u{2796}', '\u{2212}', '\u{2012}',
        '\u{2013}',
    ] {
        assert_eq!(match_key(&format!("{dash}5")).text, "-5", "{dash:?}");
    }
    // An em dash is punctuation, not a minus.
    assert_eq!(match_key("\u{2014}5").text, "\u{2014}5");
}

/// Fix round 1: every ligature step 3 maps, and the one it leaves.
#[test]
fn ligatures_fold_to_their_letters() {
    for (ligature, letters) in [
        ('\u{FB00}', "ff"),
        ('\u{FB01}', "fi"),
        ('\u{FB02}', "fl"),
        ('\u{FB03}', "ffi"),
        ('\u{FB04}', "ffl"),
        ('\u{FB06}', "st"),
    ] {
        assert_eq!(
            match_key(&format!("a{ligature}b")).text,
            format!("a{letters}b"),
            "{ligature:?}"
        );
    }
    assert_eq!(match_key("\u{FB05}").text, "\u{FB05}");
}

/// Fix round 1: on unspaced Thai without nikhahit or phinthu, normalize_th reaches the fixed
/// point PyThaiNLP's own rules reach. The plan's step order differs from PyThaiNLP's only around
/// SARA AM (recomposed in step 3, before step 4's mark swap) and phinthu (which NFC moves), and
/// only for spaces, invisible characters and digits, which this alphabet leaves out.
#[test]
fn normalize_th_agrees_with_pythainlp_on_unspaced_thai() {
    let alphabet: Vec<char> = "\u{0E01}\u{0E02}\u{0E19}\u{0E17}\u{0E24}\u{0E26}\u{0E23}\u{0E2D}\u{0E30}\u{0E31}\u{0E32}\u{0E33}\u{0E34}\u{0E35}\u{0E36}\u{0E37}\u{0E38}\u{0E39}\u{0E40}\u{0E41}\u{0E44}\u{0E45}\u{0E47}\u{0E48}\u{0E49}\u{0E4A}\u{0E4B}\u{0E4C}\u{0E4E}"
        .chars()
        .collect();
    let mut rng = Rng(400);
    let mut differences = Vec::new();
    for _ in 0..20_000 {
        let input = rng.string(&alphabet, 10);
        let mut oracle = input.clone();
        for _ in 0..8 {
            let next = pythainlp_compat(&oracle);
            if next == oracle {
                break;
            }
            oracle = next;
        }
        let normalized = normalize_th(&input);
        if normalized.stable() && normalized.text != oracle {
            differences.push(format!(
                "{input:?}: {:?} against {oracle:?}",
                normalized.text
            ));
        }
    }
    assert!(
        differences.is_empty(),
        "{} differ:\n{}",
        differences.len(),
        differences
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
