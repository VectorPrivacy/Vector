//! Kind-based tag lookups.
//!
//! nostr 0.45 removed `Tags::find` / `Tags::filter` along with the `TagKind`
//! enum; tag kinds are plain `&str` now. These restore the lookups over the new
//! representation, so the tag API lives in one place if upstream moves again.

use nostr_sdk::prelude::*;

/// Find tags by kind.
pub trait TagsExt {
    /// First tag whose kind matches, e.g. `"e"`, `"p"`, `"imeta"`.
    fn find_kind(&self, kind: &str) -> Option<&Tag>;

    /// Every tag whose kind matches.
    fn filter_kind<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a Tag> + 'a;

    /// Content of the first tag whose kind matches.
    fn kind_content(&self, kind: &str) -> Option<&str> {
        self.find_kind(kind).and_then(|t| t.content())
    }
}

impl TagsExt for Tags {
    #[inline]
    fn find_kind(&self, kind: &str) -> Option<&Tag> {
        self.iter().find(|t| t.kind() == kind)
    }

    #[inline]
    fn filter_kind<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a Tag> + 'a {
        self.iter().filter(move |t| t.kind() == kind)
    }
}

impl TagsExt for [Tag] {
    #[inline]
    fn find_kind(&self, kind: &str) -> Option<&Tag> {
        self.iter().find(|t| t.kind() == kind)
    }

    #[inline]
    fn filter_kind<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a Tag> + 'a {
        self.iter().filter(move |t| t.kind() == kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags() -> Tags {
        Tags::from_list(vec![
            Tag::custom("e", ["evt"]),
            Tag::custom("imeta", ["url http://x"]),
            Tag::custom("e", ["evt2"]),
        ])
    }

    #[test]
    fn finds_first_match_only() {
        assert_eq!(tags().kind_content("e"), Some("evt"));
        assert_eq!(tags().find_kind("nope"), None);
    }

    #[test]
    fn filters_every_match() {
        assert_eq!(tags().filter_kind("e").count(), 2);
        assert_eq!(tags().filter_kind("imeta").count(), 1);
    }
}
