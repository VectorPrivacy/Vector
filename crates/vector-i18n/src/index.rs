//! The list of languages to download, as the content of a signed Nostr event (kind 30078, `d`
//! = [`D_TAG`]). Checking the signature is the client's part; this is what it then reads.
//!
//! A key hands over by signing one last index that names its successor in [`Index::next`]: an app
//! that trusts the old key and holds the successor's own index moves its trust there for good.

use serde::{Deserialize, Serialize};

/// NIP-78 application data.
pub const KIND: u16 = 30078;
pub const D_TAG: &str = "vector-languages";
/// Bumped when a field changes meaning; an older app keeps the index it had.
pub const INDEX_FORMAT: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Index {
    pub format: u32,
    pub packs: Vec<Entry>,
    /// The hex public key that signs indexes from now on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// One downloadable language, pinned by the hash of its `.vpack`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub tag: String,
    /// In its own language.
    pub name: String,
    /// In English.
    #[serde(default)]
    pub english_name: String,
    #[serde(default)]
    pub rtl: bool,
    /// Translated share of the English it was built against, 0 to 100.
    pub coverage: u32,
    /// The [`PACK_FORMAT`](crate::PACK_FORMAT) it is written in.
    pub format: u32,
    /// Bytes, as downloaded.
    pub size: u64,
    /// Lower-case hex SHA-256 of the file: its name on Blossom and on every mirror.
    pub sha256: String,
    /// The [`english_revision`](crate::english_revision) it was measured against.
    #[serde(default)]
    pub english: String,
}

impl Index {
    /// An event's content. A newer format is refused rather than half read.
    pub fn parse(content: &str) -> Result<Self, String> {
        let i: Self = serde_json::from_str(content).map_err(|e| format!("not a language index: {e}"))?;
        if i.format > INDEX_FORMAT {
            return Err(format!("language index format {} needs a newer Vector", i.format));
        }
        Ok(i)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("an index serialises")
    }

    /// What this build can install: a pack format it reads, a tag that is a language, a hash
    /// that is one. The rest is skipped, never an error, so one odd entry can't hide the others.
    pub fn usable(&self) -> impl Iterator<Item = &Entry> {
        self.packs.iter().filter(|e| e.usable())
    }

    pub fn entry(&self, tag: &str) -> Option<&Entry> {
        self.usable().find(|e| e.tag == tag)
    }
}

impl Entry {
    pub fn usable(&self) -> bool {
        self.format <= crate::PACK_FORMAT
            && crate::pack::is_language_tag(&self.tag)
            && self.sha256.len() == 64
            && self.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }
}
