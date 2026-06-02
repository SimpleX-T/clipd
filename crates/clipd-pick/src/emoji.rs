//! Emoji catalog — thin wrapper over the `emojis` crate (Unicode 15.1,
//! ~3,800 entries with CLDR short names + group taxonomy).
//!
//! `EmojiRef` is a static reference so search results carry zero
//! allocation overhead beyond the result vec itself. Skin-tone variants
//! are deliberately not exposed in v0.2 — they come for free once we
//! wire Shift+Arrow cycling on the focused row in a follow-up.

pub type EmojiRef = &'static emojis::Emoji;

/// Search the full catalog. Empty query returns a curated default set
/// (smileys + popular hands) so the tab opens with something to look at.
pub fn search(query: &str, limit: usize) -> Vec<EmojiRef> {
    if query.trim().is_empty() {
        return default_set(limit);
    }
    let q = query.trim().to_lowercase();
    let mut hits: Vec<(usize, EmojiRef)> = emojis::iter()
        .filter_map(|e| score(&q, e).map(|s| (s, e)))
        .collect();
    hits.sort_by_key(|(s, _)| *s);
    hits.truncate(limit);
    hits.into_iter().map(|(_, e)| e).collect()
}

fn default_set(limit: usize) -> Vec<EmojiRef> {
    emojis::iter()
        .filter(|e| {
            matches!(
                e.group(),
                emojis::Group::SmileysAndEmotion | emojis::Group::PeopleAndBody
            )
        })
        .take(limit)
        .collect()
}

fn score(q: &str, e: EmojiRef) -> Option<usize> {
    let name = e.name().to_lowercase();
    if name == q {
        return Some(0);
    }
    if let Some(aliases) = e.shortcodes().next() {
        if aliases.to_lowercase() == q {
            return Some(1);
        }
    }
    if name.starts_with(q) {
        return Some(2);
    }
    for (i, alias) in e.shortcodes().enumerate() {
        let a = alias.to_lowercase();
        if a.starts_with(q) {
            return Some(10 + i);
        }
    }
    if name.contains(q) {
        return Some(30);
    }
    for (i, alias) in e.shortcodes().enumerate() {
        if alias.to_lowercase().contains(q) {
            return Some(100 + i);
        }
    }
    None
}
