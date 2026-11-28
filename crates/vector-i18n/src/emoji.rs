//! Emoji names and search words in a language: CLDR's annotations, carried by its pack as
//! `emoji.tsv` (`emoji`, tab, name, tab, words split by `|`, one emoji a line).

use std::collections::HashMap;

pub struct EmojiNames {
    by: HashMap<Box<str>, Entry>,
}

struct Entry {
    name: Box<str>,
    /// Lowercased, the name among them, for matching.
    words: Box<[Box<str>]>,
}

/// Variation selectors come and go between sources: `☺` and `☺️` are one emoji.
fn key(emoji: &str) -> String {
    emoji.chars().filter(|&c| c != '\u{FE0F}').collect()
}

impl EmojiNames {
    pub fn parse(tsv: &str) -> Self {
        let mut by = HashMap::new();
        for line in tsv.lines() {
            let mut cols = line.split('\t');
            let (Some(emoji), Some(name)) = (cols.next(), cols.next()) else { continue };
            if emoji.is_empty() || name.is_empty() {
                continue;
            }
            let mut words: Vec<Box<str>> = Vec::new();
            for w in std::iter::once(name).chain(cols.next().unwrap_or("").split('|')) {
                let w = w.trim().to_lowercase();
                if !w.is_empty() && !words.iter().any(|x| **x == *w) {
                    words.push(w.into());
                }
            }
            by.insert(key(emoji).into(), Entry { name: name.into(), words: words.into() });
        }
        Self { by }
    }

    pub fn is_empty(&self) -> bool {
        self.by.is_empty()
    }

    /// What `emoji` is called: `笑顔`.
    pub fn name(&self, emoji: &str) -> Option<&str> {
        self.by.get(key(emoji).as_str()).map(|e| &*e.name)
    }

    /// Its name and the words it is searched by, lowercased.
    pub fn words(&self, emoji: &str) -> &[Box<str>] {
        self.by.get(key(emoji).as_str()).map_or(&[], |e| &e.words)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_words_by_emoji_with_or_without_its_selector() {
        let n = EmojiNames::parse("😀\tにっこり笑う\t笑顔|顔|にっこり笑う\n☺️\tほほえむ\tHappy|顔\n\tbroken\n");
        assert_eq!(n.name("😀"), Some("にっこり笑う"));
        assert_eq!(n.name("☺"), Some("ほほえむ"));
        assert_eq!(n.words("😀").iter().map(|w| &**w).collect::<Vec<_>>(), ["にっこり笑う", "笑顔", "顔"]);
        assert_eq!(n.words("☺️").iter().map(|w| &**w).collect::<Vec<_>>(), ["ほほえむ", "happy", "顔"]);
        assert!(n.words("🐱").is_empty());
    }
}
