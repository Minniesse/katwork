use pulldown_cmark::{Event, Parser, Tag, TagEnd, TextMergeStream};
use unicode_properties::{GeneralCategory, GeneralCategoryGroup, UnicodeGeneralCategory};
use unicode_segmentation::UnicodeSegmentation;

use crate::{ids::KatId, team::name_key};

/// Resolve roster names in Markdown text, in text order, each once.
/// Mention pickers must insert a space after the name; unspaced Thai-after-Thai needs dictionary segmentation.
pub fn resolve(body: &str, names: &[(KatId, &str)]) -> Vec<KatId> {
    let keys: Vec<_> = names
        .iter()
        .map(|(id, name)| (*id, name_key(name)))
        .collect();
    let max = keys
        .iter()
        .map(|(_, key)| key.graphemes(true).count())
        .max()
        .unwrap_or(0);
    let mut result = Vec::new();
    if max == 0 {
        return result;
    }
    let mut in_code = false;
    for event in TextMergeStream::new(Parser::new(body)) {
        match event {
            Event::Start(Tag::CodeBlock(_)) => in_code = true,
            Event::End(TagEnd::CodeBlock) => in_code = false,
            Event::Text(text) if !in_code => {
                let mut consumed = 0;
                for (at, _) in text.match_indices('@') {
                    if at < consumed
                        || text[..at]
                            .chars()
                            .next_back()
                            .is_some_and(|ch| ch.is_ascii_alphanumeric() || "._%+-/@".contains(ch))
                    {
                        continue;
                    }
                    let rest = &text[at + 1..];
                    let mut found = None;
                    for (start, cluster) in rest.grapheme_indices(true) {
                        let end = start + cluster.len();
                        let candidate = &rest[..end];
                        let key = name_key(candidate);
                        if key.graphemes(true).count() > max {
                            break;
                        }
                        if candidate.ends_with(char::is_whitespace)
                            || !ends_mention(candidate, &rest[end..])
                        {
                            continue;
                        }
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

fn ends_mention(candidate: &str, rest: &str) -> bool {
    let Some(next) = rest.chars().next() else {
        return true;
    };
    if next.is_whitespace()
        || next.general_category() == GeneralCategory::Format
        || next.general_category_group() == GeneralCategoryGroup::Punctuation
    {
        return true;
    }
    let thai = |ch: char| ('\u{0e00}'..='\u{0e7f}').contains(&ch);
    let previous = candidate
        .graphemes(true)
        .next_back()
        .and_then(|g| g.chars().next())
        .unwrap();
    (thai(previous) && !thai(next) && next.is_alphanumeric())
        || (!thai(previous) && previous.is_alphanumeric() && thai(next) && next.is_alphanumeric())
}
