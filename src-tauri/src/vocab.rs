//! Custom words: names and terms the models tend to mishear ("GECK", "Skyrim"). One entry per line in
//! Settings: `GECK` fixes the spelling and capitals of any heard "geck"; `GECK = jeck, gek` also turns
//! those sound-alikes into GECK; several words are fine on either side (`Garden of Eden = garden of eaten`).
//! It runs on the finished words from either pass, so it adds no delay.

use std::sync::RwLock;

struct Entry {
    heard: Vec<String>,
    write: String,
}

static ENTRIES: RwLock<Vec<Entry>> = RwLock::new(Vec::new());

fn key(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

fn parse(text: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    for line in text.lines() {
        let (write, heard) = line.split_once('=').unwrap_or((line, ""));
        let write = write.trim();
        if write.is_empty() {
            continue;
        }
        let spoken = std::iter::once(write).chain(heard.split(',').map(str::trim));
        for h in spoken {
            let heard: Vec<String> = h.split_whitespace().map(key).filter(|w| !w.is_empty()).collect();
            if !heard.is_empty() {
                entries.push(Entry { heard, write: write.to_string() });
            }
        }
    }
    // Longest phrases first, so "garden of eden" wins over "garden".
    entries.sort_by_key(|e| std::cmp::Reverse(e.heard.len()));
    entries
}

/// Replaces the list with the one in `text` (the Settings box).
pub fn set(text: &str) {
    *ENTRIES.write().unwrap_or_else(|e| e.into_inner()) = parse(text);
}

/// Rewrites heard words to the user's spellings, keeping punctuation next to the words.
pub fn apply(words: Vec<String>) -> Vec<String> {
    let entries = ENTRIES.read().unwrap_or_else(|e| e.into_inner());
    if entries.is_empty() {
        return words;
    }
    let keys: Vec<String> = words.iter().map(|w| key(w)).collect();
    let mut out = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        let hit = entries.iter().find(|e| i + e.heard.len() <= words.len() && keys[i..i + e.heard.len()] == e.heard[..]);
        match hit {
            Some(e) => {
                let last = &words[i + e.heard.len() - 1];
                let first = &words[i];
                let lead = &first[..first.len() - first.trim_start_matches(|c: char| !c.is_alphanumeric()).len()];
                let tail = &last[last.trim_end_matches(|c: char| !c.is_alphanumeric()).len()..];
                let parts: Vec<&str> = e.write.split_whitespace().collect();
                for (n, part) in parts.iter().enumerate() {
                    let before = if n == 0 { lead } else { "" };
                    let after = if n + 1 == parts.len() { tail } else { "" };
                    out.push(format!("{before}{part}{after}"));
                }
                i += e.heard.len();
            }
            None => {
                out.push(words[i].clone());
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn fixes_spelling_aliases_phrases_and_keeps_punctuation() {
        set("GECK = jeck, gek\nGarden of Eden = garden of eaten\nSkyrim");
        assert_eq!(apply(words("the geck is great.")), words("the GECK is great."));
        assert_eq!(apply(words("use the jeck, then gek")), words("use the GECK, then GECK"));
        assert_eq!(apply(words("In the garden of eaten, skyrim.")), words("In the Garden of Eden, Skyrim."));
        assert_eq!(apply(words("a gecko")), words("a gecko"));
        set("");
        assert_eq!(apply(words("geck")), words("geck"));
    }
}
