//! A language pack as it travels and is cached: one deflated JSON document holding the
//! manifest, the language's number and date data, and its `.ftl` files.

use std::collections::BTreeMap;
use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

use crate::{LocaleData, Pack};

/// Bumped when a field changes meaning; an older app refuses a newer pack.
pub const PACK_FORMAT: u32 = 1;
/// Inflated packs are tens of KB; anything near this is not one.
const MAX_INFLATED: u64 = 16 << 20;

#[derive(Serialize, Deserialize)]
struct Wire {
    format: u32,
    tag: String,
    name: String,
    #[serde(default)]
    english_name: String,
    rtl: bool,
    english: String,
    coverage: u32,
    #[serde(default)]
    locale: LocaleData,
    files: BTreeMap<String, String>,
}

/// The built-in English a pack is measured against, so a pack can tell it was built for an
/// older one.
pub fn english_revision() -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (name, src) in crate::EN_GB {
        for b in name.bytes().chain([0]).chain(src.bytes()).chain([0]) {
            h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// A tag a pack may carry: never the built-in English, never a path.
pub(crate) fn is_language_tag(tag: &str) -> bool {
    !tag.is_empty() && tag.len() <= 35 && tag != crate::BUILT_IN && tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

impl Pack {
    pub fn to_bytes(&self) -> Vec<u8> {
        let wire = Wire {
            format: PACK_FORMAT,
            tag: self.tag.clone(),
            name: self.name.clone(),
            english_name: self.english_name.clone(),
            rtl: self.rtl,
            english: self.english.clone(),
            coverage: self.coverage,
            locale: self.data.clone(),
            files: self.files.iter().cloned().collect(),
        };
        let json = serde_json::to_vec(&wire).expect("a pack serialises");
        let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
        z.write_all(&json).expect("writing to memory");
        z.finish().expect("writing to memory")
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let mut json = Vec::new();
        flate2::read::DeflateDecoder::new(bytes)
            .take(MAX_INFLATED)
            .read_to_end(&mut json)
            .map_err(|e| format!("not a language pack: {e}"))?;
        let w: Wire = serde_json::from_slice(&json).map_err(|e| format!("not a language pack: {e}"))?;
        if w.format > PACK_FORMAT {
            return Err(format!("pack format {} needs a newer Vector", w.format));
        }
        if !is_language_tag(&w.tag) {
            return Err(format!("pack tag {:?} is not a language", w.tag));
        }
        Ok(Self {
            tag: w.tag,
            name: w.name,
            english_name: w.english_name,
            rtl: w.rtl,
            english: w.english,
            coverage: w.coverage.min(100),
            files: w.files.into_iter().collect(),
            data: w.locale,
        })
    }
}
