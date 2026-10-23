use katwork::{chat::mentions::resolve, ids::KatId, team::roles, types::Locale};

fn check(body: &str, names: &[&str], expected: &[usize]) {
    let roster: Vec<_> = names.iter().map(|name| (KatId::new(), *name)).collect();
    assert_eq!(
        resolve(body, &roster),
        expected.iter().map(|i| roster[*i].0).collect::<Vec<_>>(),
        "{body:?}"
    );
}

#[test]
fn mentions_resolve_english_and_thai() {
    for locale in [Locale::En, Locale::Th] {
        let roster = roles::default_roster(locale);
        let names: Vec<_> = roster.iter().map(|kat| kat.name.as_str()).collect();
        for (index, kat) in roster.iter().enumerate() {
            check(&format!("@{} ช่วยสรุป", kat.name), &names, &[index]);
        }
    }
}

#[test]
fn mentions_roster_thai_names_survive() {
    for kat in roles::default_roster(Locale::Th) {
        for ending in [" ดูไฟล์", ","] {
            check(&format!("@{}{ending}", kat.name), &[&kat.name], &[0]);
        }
    }
}

#[test]
fn mentions_ignore_code_and_trailing_punctuation() {
    check("`@Writer` and @Analyst,", &["Writer", "Analyst"], &[1]);
}

#[test]
fn mentions_ignore_every_code_form() {
    for body in [
        "~~~\n@Writer\n~~~",
        "    @Writer",
        "``@Writer ` hi``",
        "````\n```\n@Writer\n````",
        "```\n@Writer",
        "<span title=\"@Writer\">x</span>",
        "[x](https://example.test/@Writer)",
    ] {
        check(body, &["Writer"], &[]);
    }
    check("~~~\n@Writer\n~~~\n@Analyst", &["Writer", "Analyst"], &[1]);
    check("@Wr&#105;ter", &["Writer"], &[0]);
}

#[test]
fn mentions_resolve_latin_name_then_thai_without_space() {
    check("@Writerช่วยสรุป", &["Writer"], &[0]);
    check("@เลขาWrite", &["เลขา"], &[0]);
}

#[test]
fn mentions_resolve_thai_prefix_is_not_a_match() {
    check("@เลขานุการ ดู", &["เลขา"], &[]);
    check("@เลขา ดู", &["เลขา"], &[0]);
    check("@Writerly", &["Writer"], &[]);
}

#[test]
fn mentions_email_and_url_are_not_mentions() {
    for body in [
        "a@Writer.com",
        "https://x.test/@Writer",
        ".@Writer",
        "@@Writer",
    ] {
        check(body, &["Writer"], &[]);
    }
    check("ถาม@Writer", &["Writer"], &[0]);
}

#[test]
fn mentions_zwsp_and_nbsp_end_the_token() {
    for body in ["@Writer\u{200B}ช่วย", "@Writer\u{00A0}ช่วย"] {
        check(body, &["Writer"], &[0]);
    }
}

#[test]
fn mentions_resolve_normalizes_case_and_mark_order() {
    check("@WRITER", &["Writer"], &[0]);
    check("@ก\u{0e48}\u{0e38}", &["กุ่"], &[0]);
    check("@ก\u{0e48}\u{0e34}", &["กิ่"], &[0]);
    check("@STRASSE", &["Straße"], &[0]);
}

#[test]
fn mentions_resolve_folds_whitespace_and_compatibility_forms() {
    check("@AI  Writer ช่วยดู", &["AI", "AI Writer"], &[1]);
    check("@ffi ดู", &["ﬃ"], &[0]);
    check("@ﬃ ดู", &["ffi"], &[0]);
}

#[test]
fn mentions_resolve_multi_word_name_longest_wins() {
    check("@นักวิเคราะห์ ภาษี ช่วยดู", &["นักวิเคราะห์", "นักวิเคราะห์ ภาษี"], &[1]);
}

#[test]
fn mentions_keep_text_order_and_drop_repeats() {
    check("@Analyst @Writer @Analyst", &["Writer", "Analyst"], &[1, 0]);
}

#[test]
fn mentions_preserve_emoji_graphemes_and_require_a_boundary() {
    check("👩‍💻 @Writer!", &["Writer"], &[0]);
    check("@Writer👩‍💻", &["Writer"], &[0]);
    check("@Writer 👩‍💻", &["Writer", "Writer 👩‍💻"], &[1]);
    check("@Writer", &[], &[]);
}

#[test]
fn mentions_emoji_and_symbols_end_names() {
    for (name, ending) in [
        ("Writer", "🙏"),
        ("เลขา", "🙏"),
        ("Lead", "✅"),
        ("Writer", "👩‍💻"),
        ("Writer", "+"),
        ("writer", "~"),
        ("Writer", "©️"),
        ("Writer", "1️⃣"),
        ("Writer", "\u{1fc00}"),
    ] {
        check(&format!("@{name}{ending}"), &[name], &[0]);
    }
    check("@Writer+@Analyst", &["Writer", "Analyst"], &[0]);
    check("@Writer1", &["Writer"], &[]);
}

#[test]
fn mentions_whitespace_after_at_is_not_a_mention() {
    for body in [
        "@ Writer",
        "meet @ Lead's office",
        "@\u{00a0}Writer",
        "@\tWriter",
    ] {
        check(body, &["Writer", "Lead"], &[]);
    }
}

#[test]
fn mentions_email_and_url_prefixes_are_not_mentions() {
    for prefix in "aZ09=/?&.:%+-_#~".chars() {
        check(&format!("{prefix}@Writer"), &["Writer"], &[]);
    }
    check("bob@Writer.com ?u=@Writer", &["Writer"], &[]);
    check("ถาม@Writer é@Writer", &["Writer"], &[0]);
}

#[test]
fn mentions_thai_continuation_is_not_a_boundary() {
    check("@เลขาช่วยดู", &["เลขา"], &[]);
    check("@เลขานุการ", &["เลขา", "เลขานุการ"], &[1]);
}

#[test]
fn mentions_normalization_can_compose_across_raw_graphemes() {
    check("@ㄱㅏ", &["가"], &[0]);
    check("@a\u{00a8}", &["a \u{0308}"], &[0]);
    check("@ก\u{0e48}\u{0e34}\u{0e34}", &["กิ่"], &[0]);
}

#[test]
fn mentions_resolve_long_whitespace_runs_within_normalized_bound() {
    let body = format!("@AI{}Writer", " ".repeat(30_000));
    check(&body, &["AI", "AI Writer"], &[1]);
    check(&format!("@Writer{}", " ".repeat(30_000)), &["Writer"], &[0]);
}
