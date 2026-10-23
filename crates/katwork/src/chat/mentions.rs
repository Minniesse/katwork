use std::{
    collections::{BTreeMap, HashSet},
    sync::{
        LazyLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use pulldown_cmark::{Event, Parser, Tag, TagEnd, TextMergeStream};
use regex_syntax::hir::{Class, ClassUnicode, HirKind};
use unicode_normalization::char::{canonical_combining_class, compose};
use unicode_properties::{GeneralCategory, GeneralCategoryGroup, UnicodeGeneralCategory};
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    ids::KatId,
    team::{fold_name, name_key},
};

#[derive(Default)]
struct Node {
    next: BTreeMap<char, usize>,
    kat: Option<KatId>,
}

#[derive(Clone, Default)]
struct Prefix {
    node: usize,
    space: bool,
}

impl Prefix {
    fn extend(&mut self, text: &str, trie: &[Node]) -> Option<()> {
        for ch in text.chars() {
            if ch.is_whitespace() {
                self.space = self.node != 0;
                continue;
            }
            if self.space {
                self.node = *trie[self.node].next.get(&' ')?;
                self.space = false;
            }
            self.node = *trie[self.node].next.get(&ch)?;
        }
        Some(())
    }
}

/// Resolve roster names in Markdown text, in text order, each once.
/// Mention pickers insert a trailing space; Thai-after-Thai needs dictionary segmentation.
/// Candidate normalization has a shared byte-work budget of 128 times the body length.
/// Exhaustion returns no mentions, so routing falls back to the Lead without partial recipients.
pub fn resolve(body: &str, names: &[(KatId, &str)]) -> Vec<KatId> {
    let mut trie = vec![Node::default()];
    for (id, name) in names {
        let mut node = 0;
        for ch in name_key(name).chars() {
            let next = trie.len();
            node = *trie[node].next.entry(ch).or_insert(next);
            if node == next {
                trie.push(Node::default());
            }
        }
        trie[node].kat.get_or_insert(*id);
    }
    let mut result = Vec::new();
    if trie.len() == 1 {
        return result;
    }
    let mut seen = HashSet::new();
    let mut budget = body.len().saturating_mul(128);
    let mut in_code = false;
    for event in TextMergeStream::new(Parser::new(body)) {
        match event {
            Event::Start(Tag::CodeBlock(_)) => in_code = true,
            Event::End(TagEnd::CodeBlock) => in_code = false,
            Event::Text(text) if !in_code => {
                let mut consumed = 0;
                for (at, _) in text.match_indices('@') {
                    if at < consumed
                        || text[..at].chars().next_back().is_some_and(|ch| {
                            ch.is_ascii_alphanumeric() || "=/?&.:%+-_#~@".contains(ch)
                        })
                    {
                        continue;
                    }
                    let rest = &text[at + 1..];
                    if rest.starts_with(char::is_whitespace) {
                        continue;
                    }
                    let mut prefix = Prefix::default();
                    let mut tail_start = 0;
                    let mut tail = String::new();
                    let mut found = None;
                    for (start, cluster) in rest.grapheme_indices(true) {
                        let end = start + cluster.len();
                        let Some(chunk) = fold_with_budget(cluster, &mut budget) else {
                            return exhausted(body.len());
                        };
                        // A non-mark starter that cannot compose seals the preceding raw suffix.
                        // Repeated Hangul L/V jamo need not wait for a grapheme boundary.
                        if chunk.chars().next().is_some_and(|first| {
                            canonical_combining_class(first) == 0
                                && first.general_category_group() != GeneralCategoryGroup::Mark
                                && tail
                                    .chars()
                                    .next_back()
                                    .is_none_or(|last| compose(last, first).is_none())
                        }) {
                            if prefix.extend(&tail, &trie).is_none() {
                                break;
                            }
                            tail_start = start;
                            tail = chunk;
                        } else {
                            // Retain raw input: Thai sorting/deduplication must run only once per prefix.
                            let Some(folded) =
                                fold_with_budget(&rest[tail_start..end], &mut budget)
                            else {
                                return exhausted(body.len());
                            };
                            tail = folded;
                        }
                        let mut candidate = prefix.clone();
                        if !cluster.ends_with(char::is_whitespace)
                            && candidate.extend(&tail, &trie).is_some()
                            && let Some(id) = trie[candidate.node].kat
                            && ends_mention(cluster, &rest[end..])
                        {
                            found = Some((id, end));
                        }
                    }
                    if let Some((id, end)) = found {
                        consumed = at + 1 + end;
                        if seen.insert(id) {
                            result.push(id);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    result
}

fn fold_with_budget(text: &str, budget: &mut usize) -> Option<String> {
    *budget = budget.checked_sub(text.len())?;
    #[cfg(test)]
    tests::count_work(text.len());
    let folded = fold_name(text);
    *budget = budget.checked_sub(folded.len())?;
    Some(folded)
}

fn exhausted(body_bytes: usize) -> Vec<KatId> {
    static START: LazyLock<Instant> = LazyLock::new(Instant::now);
    static LAST_WARN: AtomicU64 = AtomicU64::new(0);
    if should_warn(&LAST_WARN, START.elapsed().as_secs() + 1) {
        // Intake supplies thread_id in the current span; never record message text.
        tracing::warn!(body_bytes, "mention normalization budget exhausted");
    }
    Vec::new()
}

fn should_warn(last: &AtomicU64, now: u64) -> bool {
    last.try_update(Ordering::Relaxed, Ordering::Relaxed, |previous| {
        (previous == 0 || now.saturating_sub(previous) >= 60).then_some(now)
    })
    .is_ok()
}

fn emoji(cluster: &str) -> bool {
    static EMOJI: LazyLock<ClassUnicode> = LazyLock::new(|| {
        let hir = regex_syntax::Parser::new()
            .parse(r"[\p{Extended_Pictographic}\p{Emoji_Presentation}]")
            .expect("valid Unicode emoji properties");
        let HirKind::Class(Class::Unicode(class)) = hir.kind() else {
            unreachable!()
        };
        class.clone()
    });
    let ch = cluster.chars().next().unwrap();
    let ranges = EMOJI.ranges();
    let index = ranges.partition_point(|range| range.end() < ch);
    ranges.get(index).is_some_and(|range| range.start() <= ch)
        || ("0123456789#*".contains(ch)
            && (cluster.ends_with('\u{20e3}') || cluster.chars().nth(1) == Some('\u{fe0f}')))
}

fn ends_mention(previous: &str, rest: &str) -> bool {
    let Some(cluster) = rest.graphemes(true).next() else {
        return true;
    };
    let next = cluster.chars().next().unwrap();
    if next.is_whitespace()
        || next.general_category() == GeneralCategory::Format
        || matches!(
            next.general_category_group(),
            GeneralCategoryGroup::Punctuation | GeneralCategoryGroup::Symbol
        )
        || emoji(cluster)
    {
        return true;
    }
    let thai = |ch: char| ('\u{0e00}'..='\u{0e7f}').contains(&ch);
    let previous = previous.chars().next().unwrap();
    (thai(previous) && !thai(next) && next.is_alphanumeric())
        || (!thai(previous) && previous.is_alphanumeric() && thai(next) && next.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    thread_local! {
        static WORK: Cell<usize> = const { Cell::new(0) };
    }

    // Deliberately normalize each whole prefix, independently of the incremental trie walk.
    fn reference(body: &str, names: &[(KatId, &str)]) -> Vec<KatId> {
        let keys: Vec<_> = names
            .iter()
            .map(|(id, name)| (*id, name_key(name)))
            .collect();
        let mut result = Vec::new();
        let mut in_code = false;
        for event in TextMergeStream::new(Parser::new(body)) {
            match event {
                Event::Start(Tag::CodeBlock(_)) => in_code = true,
                Event::End(TagEnd::CodeBlock) => in_code = false,
                Event::Text(text) if !in_code => {
                    let mut consumed = 0;
                    for (at, _) in text.match_indices('@') {
                        if at < consumed
                            || text[..at].chars().next_back().is_some_and(|ch| {
                                ch.is_ascii_alphanumeric() || "=/?&.:%+-_#~@".contains(ch)
                            })
                        {
                            continue;
                        }
                        let rest = &text[at + 1..];
                        if rest.starts_with(char::is_whitespace) {
                            continue;
                        }
                        let mut found = None;
                        for (start, cluster) in rest.grapheme_indices(true) {
                            let end = start + cluster.len();
                            if cluster.ends_with(char::is_whitespace)
                                || !ends_mention(cluster, &rest[end..])
                            {
                                continue;
                            }
                            let key = name_key(&rest[..end]);
                            if let Some((id, _)) = keys.iter().find(|(_, name)| *name == key) {
                                found = Some((*id, end));
                            }
                        }
                        if let Some((id, end)) = found {
                            consumed = at + 1 + end;
                            if !result.contains(&id) {
                                result.push(id);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        result
    }

    #[test]
    fn mentions_match_whole_prefix_normalization() {
        let atoms = [
            "a",
            "B",
            "ß",
            "é",
            "\u{0308}",
            "\u{0301}",
            "ก",
            "ข",
            "\u{0e38}",
            "\u{0e34}",
            "\u{0e48}",
            "\u{0e4e}",
            "\u{3099}",
            "ㄱ",
            "ㅏ",
            "ㅋ",
            "ㅎ",
            "ㅠ",
            "\u{1100}",
            "\u{1161}",
            "\u{11a8}",
            "가",
            "ﬃ",
            "\u{fdfa}",
            "\u{00a8}",
            "🙏",
            "👩‍💻",
            "🇹🇭",
            "🇺",
            "🇸",
            "1️⃣",
            "1\u{fe0f}",
            "1\u{fe0e}",
            "©️",
            " ",
            "\t",
            "\n",
            "\r",
            "\u{000b}",
            "\u{000c}",
            "\u{0085}",
            "\u{00a0}",
            "\u{1680}",
            "\u{2000}",
            "\u{2001}",
            "\u{2002}",
            "\u{2003}",
            "\u{2004}",
            "\u{2005}",
            "\u{2006}",
            "\u{2007}",
            "\u{2008}",
            "\u{2009}",
            "\u{200a}",
            "\u{2028}",
            "\u{2029}",
            "\u{202f}",
            "\u{205f}",
            "\u{3000}",
            "\u{200b}",
        ];
        let mut seed = 0x6a09_e667_f3bc_c909_u64;
        let mut pick = |bound| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as usize % bound
        };
        for case in 0..20_000 {
            let mut literal = String::new();
            for _ in 0..1 + pick(8) {
                literal.push_str(atoms[pick(atoms.len())]);
            }
            // Keep both review shapes in the corpus, alongside random mixed-script strings.
            if case % 100 == 0 {
                literal = "ก\u{0e38}\u{0e34}\u{0e38}\u{3099}ข".into();
            } else if case % 100 == 1 {
                literal = "ß\u{0e38}\u{0308}\u{0e4e}\u{0e38}".into();
            }
            let once = fold_name(&literal);
            assert_eq!(fold_name(&once), once, "case {case}: {literal:?}");
            let folded = name_key(&literal);
            let longer = format!("{literal} {}", atoms[pick(atoms.len())]);
            let short = literal.graphemes(true).next().unwrap();
            let roster = [
                (KatId::new(), literal.as_str()),
                (KatId::new(), folded.as_str()),
                (KatId::new(), longer.as_str()),
                (KatId::new(), short),
                (KatId::new(), "Writer"),
            ];
            let spelling = [literal.as_str(), folded.as_str(), longer.as_str()][pick(3)];
            let prefix = ["", "ถาม", "x", "?u=", " "][pick(5)];
            let body = format!(
                "@Writer {prefix}@{spelling}{} @{} `@Writer`",
                atoms[pick(atoms.len())],
                literal
            );
            assert_eq!(
                resolve(&body, &roster),
                reference(&body, &roster),
                "case {case}: {body:?}; {roster:?}"
            );
        }
    }

    #[test]
    fn mention_budget_warning_is_rate_limited() {
        let last = AtomicU64::new(0);
        assert!(should_warn(&last, 1));
        assert!(!should_warn(&last, 1));
        assert!(!should_warn(&last, 60));
        assert!(should_warn(&last, 61));
        assert!(!should_warn(&last, 61));
    }

    pub(super) fn count_work(bytes: usize) {
        WORK.with(|work| {
            work.set(work.get() + bytes);
        });
    }

    #[test]
    fn mentions_exhausted_budget_returns_no_partial_mentions() {
        // Deliberately bypass name validation to exercise exhaustion with overlapping prefixes.
        let name = format!("a {}z", "＠a ".repeat(79));
        let body = format!("@Done {}", "@a ".repeat(10_000));
        let roster = [(KatId::new(), "Done"), (KatId::new(), name.as_str())];
        WORK.with(|work| work.set(0));
        let resolved = resolve(&body, &roster);
        WORK.with(|work| assert!(work.get() > 32 * body.len(), "must exhaust the budget"));
        assert!(resolved.is_empty());
        WORK.with(|work| assert!(work.get() <= 128 * body.len()));
    }

    #[test]
    fn mentions_normalization_work_is_bounded_for_adversarial_bodies() {
        for name in ["a".repeat(120), "ก".repeat(120), "\u{fdfa}".repeat(120)] {
            for fragment in ["@ ", "@a ", "@ก ", "@\u{fdfa} ", "@a\u{fdfa} "] {
                let body = format!("{}@Done", fragment.repeat((32_768 - 5) / fragment.len()));
                let roster = [(KatId::new(), name.as_str()), (KatId::new(), "Done")];
                WORK.with(|work| work.set(0));
                assert_eq!(resolve(&body, &roster), vec![roster[1].0]);
                WORK.with(|work| assert!(work.get() <= 32 * body.len()));
            }
        }
    }
}
