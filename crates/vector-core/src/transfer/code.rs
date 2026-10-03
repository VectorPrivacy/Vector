//! The transfer code, `<nameplate>-<word>-<word>-<word>`. The nameplate picks the meeting room
//! and is public; the words are the secret the two devices prove to each other.

use bip39::Language;
use rand::Rng;
use zeroize::Zeroizing;

/// Secret words per code: 11 bits each from the BIP-39 English list.
pub const WORDS: usize = 3;
/// Nameplates run 1..=NAMEPLATE_MAX. Short to type, and only codes in use at once can meet.
pub const NAMEPLATE_MAX: u16 = 999;
/// What the QR carries ahead of the code. Plain text, not a link: no camera app or link handler
/// can open it, and someone scanning it elsewhere reads a code they can type.
pub const QR_PREFIX: &str = "Vector transfer code: ";

/// Words a generated code never uses: nothing violent, intimate, insulting, or that pairs with
/// another into something worse. A typed code still accepts every BIP-39 word.
const AVOIDED: &[&str] = &[
    "abuse", "accident", "accuse", "acid", "addict", "adult", "affair", "alcohol", "anger",
    "angry", "anxiety", "armed", "arrest", "assault", "asthma", "attack", "awful", "baby", "ball",
    "banana", "battle", "betray", "blade", "blast", "blind", "blood", "blush", "body", "bomb",
    "bone", "bottom", "boy", "bullet", "cannon", "casino", "cherry", "chest", "child", "chronic",
    "cigar", "crash", "crazy", "cream", "crime", "cruel", "damage", "danger", "demise", "despair",
    "destroy", "disease", "disorder", "divorce", "dose", "drink", "dumb", "dutch", "enemy", "evil",
    "execute", "exotic", "expose", "fat", "fatal", "female", "fever", "flush", "fury", "girl",
    "goddess", "gorilla", "gospel", "grief", "guilt", "gun", "hazard", "hip", "hole", "horn",
    "horror", "hurt", "husband", "illegal", "illness", "infant", "inflict", "inhale", "inject",
    "injury", "inmate", "insane", "jealous", "kid", "kiss", "knife", "lady", "latin", "lazy",
    "liar", "load", "lonely", "mad", "maid", "man", "meat", "misery", "monkey", "nasty", "nut",
    "orphan", "panic", "pill", "pistol", "pole", "poverty", "prison", "punch", "ride", "rifle",
    "riot", "romance", "rude", "sad", "sadness", "sausage", "shaft", "shoot", "sick", "siege",
    "skin", "skull", "smoke", "sniff", "squeeze", "suffer", "suspect", "swallow", "swear", "sword",
    "symptom", "tobacco", "toddler", "toilet", "tongue", "tragic", "trap", "trigger", "ugly",
    "useless", "vicious", "virus", "warfare", "weapon", "weird", "wet", "whip", "wife", "wine",
    "wink", "woman", "wreck",
];
/// Nameplates with a reading of their own.
const AVOIDED_NAMEPLATES: &[u16] = &[14, 69, 88, 420, 666, 911];

/// A transfer code. Debug never prints the words.
#[derive(Clone, PartialEq, Eq)]
pub struct Code {
    nameplate: u16,
    words: [u16; WORDS],
}

impl std::fmt::Debug for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Code({}-…)", self.nameplate)
    }
}

impl Code {
    pub fn generate() -> Self {
        let mut rng = rand::rngs::OsRng;
        let list = Language::English.word_list();
        // Redraw an avoided word: still uniform over the rest, and nothing kept in memory.
        let words = std::array::from_fn(|_| loop {
            let i = rng.gen_range(0..2048u16);
            if !AVOIDED.contains(&list[i as usize]) {
                break i;
            }
        });
        let nameplate = loop {
            let n = rng.gen_range(1..=NAMEPLATE_MAX);
            if !AVOIDED_NAMEPLATES.contains(&n) {
                break n;
            }
        };
        Self { nameplate, words }
    }

    /// Read a typed or scanned code. Case and separators are forgiven, and a word may be cut to
    /// its first four letters, which are unique in the BIP-39 list.
    pub fn parse(input: &str) -> Result<Self, String> {
        const BAD: &str = "That code doesn't look right. It's a number and three words, like 7-orbit-lemon-stage.";
        let lower = input.trim().to_lowercase();
        let body = lower.strip_prefix(&QR_PREFIX.to_lowercase()).unwrap_or(&lower);
        let mut parts = body.split(|c: char| !c.is_alphanumeric()).filter(|p| !p.is_empty());
        let nameplate = parts
            .next()
            .and_then(|p| p.parse::<u16>().ok())
            .filter(|n| (1..=NAMEPLATE_MAX).contains(n))
            .ok_or(BAD)?;
        let mut words = [0u16; WORDS];
        for slot in words.iter_mut() {
            let part = parts.next().ok_or(BAD)?;
            *slot = word_index(part).ok_or_else(|| format!("\"{part}\" isn't one of the code words. Check the spelling."))?;
        }
        if parts.next().is_some() {
            return Err(BAD.to_string());
        }
        Ok(Self { nameplate, words })
    }

    pub fn nameplate(&self) -> u16 {
        self.nameplate
    }

    /// Lowercase, full words, `-` joined: what both devices feed the PAKE.
    pub fn canonical(&self) -> Zeroizing<String> {
        let list = Language::English.word_list();
        let mut out = Zeroizing::new(self.nameplate.to_string());
        for w in self.words {
            out.push('-');
            out.push_str(list[w as usize]);
        }
        out
    }

    /// The QR payload.
    pub fn qr(&self) -> Zeroizing<String> {
        Zeroizing::new(format!("{QR_PREFIX}{}", self.canonical().as_str()))
    }
}

fn word_index(part: &str) -> Option<u16> {
    let lang = Language::English;
    if let Some(i) = lang.find_word(part) {
        return Some(i);
    }
    if part.len() < 4 {
        return None;
    }
    match lang.words_by_prefix(part) {
        [only] => lang.find_word(only),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_reads_back_however_it_is_typed() {
        for _ in 0..200 {
            let code = Code::generate();
            let canon = code.canonical();
            assert_eq!(Code::parse(&canon).unwrap(), code);
            assert_eq!(Code::parse(&code.qr()).unwrap(), code);
            assert_eq!(Code::parse(&canon.replace('-', "\u{2013}")).unwrap(), code, "en dashes");
            assert_eq!(Code::parse(&canon.replace('-', "\u{a0}")).unwrap(), code, "no-break spaces");
            assert_eq!(Code::parse(&format!("  {}  ", canon.to_uppercase().replace('-', "  "))).unwrap(), code);
            let short: Vec<String> = canon.split('-').map(|p| p.chars().take(4).collect()).collect();
            assert_eq!(Code::parse(&short.join("-")).unwrap(), code, "four-letter prefixes are unique");
        }
    }

    #[test]
    fn a_malformed_code_is_refused() {
        for bad in ["", "7", "7-orbit-lemon", "7-orbit-lemon-stage-vivid", "0-orbit-lemon-stage", "000-orbit-lemon-stage",
                    "1000-orbit-lemon-stage", "x-orbit-lemon-stage", "7-orbit-lemon-notaword", "7-orb-lemon-stage",
                    "vector://transfer#7-orbit-lemon-stage", "７-orbit-lemon-stage"] {
            assert!(Code::parse(bad).is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn a_leading_zero_or_a_word_that_prefixes_others_reads_the_same() {
        assert_eq!(Code::parse("007-orbit-lemon-stage").unwrap().canonical().as_str(), "7-orbit-lemon-stage");
        // "act" is a word and also begins "action": the whole word wins.
        assert_eq!(Code::parse("7-act-act-act").unwrap().canonical().as_str(), "7-act-act-act");
        assert_eq!(Code::parse("7-acti-lemon-stage").unwrap().canonical().as_str(), "7-action-lemon-stage");
    }

    #[test]
    fn a_generated_code_never_uses_an_avoided_word_or_number() {
        for w in AVOIDED {
            assert!(Language::English.find_word(w).is_some(), "{w} isn't a BIP-39 word");
        }
        let usable = Language::English.word_list().iter().filter(|w| !AVOIDED.contains(w)).count();
        assert!(usable >= 1850, "enough words left for ~32.6 bits");
        for _ in 0..20_000 {
            let code = Code::generate();
            let canon = code.canonical();
            let mut parts = canon.split('-');
            let nameplate: u16 = parts.next().unwrap().parse().unwrap();
            assert!(!AVOIDED_NAMEPLATES.contains(&nameplate), "{nameplate}");
            for w in parts {
                assert!(!AVOIDED.contains(&w), "{w}");
            }
        }
        // Typing still accepts them, so a code from anywhere reads back.
        assert!(Code::parse("7-wine-knife-gun").is_ok());
    }

    #[test]
    fn debug_hides_the_words() {
        let code = Code::parse("7-orbit-lemon-stage").unwrap();
        let shown = format!("{code:?}");
        assert!(!shown.contains("orbit") && shown.contains('7'));
    }
}
