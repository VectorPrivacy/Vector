//! Events read from JSON: nostr-parse first, rust-nostr's reader for anything it refuses.
//!
//! nostr-parse is differential-tested against `Event::from_json` and `UnsignedEvent::from_json`:
//! whatever it accepts, rust-nostr reads identically. It refuses a few spellings rust-nostr
//! takes (uppercase hex, bech32 keys), and an empty tag fails the conversion; those go to
//! rust-nostr, errors included, so these return exactly what `from_json` would.

use std::cell::RefCell;

use nostr_sdk::prelude::{Event, EventId, Kind, PublicKey, Signature, Tag, Timestamp, UnsignedEvent};

thread_local! {
    // Scratch space reused across parses; nothing in it outlives one.
    static PARSER: RefCell<nostr_parse::Parser> = RefCell::new(nostr_parse::Parser::new());
}

/// A signed event (a seal, a wrap, anything relay-shaped), as `Event::from_json` reads it.
pub fn event(json: &str) -> Result<Event, nostr::error::Error> {
    fast_event(json).map_or_else(|| Event::from_json(json), Ok)
}

/// An unsigned event (a NIP-59 rumor), as `UnsignedEvent::from_json` reads it.
pub fn unsigned(json: &str) -> Result<UnsignedEvent, nostr::error::Error> {
    fast_unsigned(json).map_or_else(|| UnsignedEvent::from_json(json), Ok)
}

fn fast_event(json: &str) -> Option<Event> {
    let e = PARSER.with(|p| p.borrow_mut().parse(json)).ok()?;
    let sig = Signature::from_byte_array(e.sig?);
    Some(Event::new(
        EventId::from_byte_array(e.id),
        PublicKey::from_byte_array(e.pubkey),
        Timestamp::from_secs(e.created_at),
        Kind::from_u16(e.kind),
        tags(e.tags)?,
        e.content,
        sig,
    ))
}

fn fast_unsigned(json: &str) -> Option<UnsignedEvent> {
    let e = PARSER.with(|p| p.borrow_mut().parse_unsigned(json)).ok()?;
    let mut rumor = UnsignedEvent::new(
        PublicKey::from_byte_array(e.pubkey),
        Timestamp::from_secs(e.created_at),
        Kind::from_u16(e.kind),
        tags(e.tags)?,
        e.content,
    );
    rumor.id = e.id.map(EventId::from_byte_array);
    Some(rumor)
}

/// Moves the strings; `None` for an empty tag, which rust-nostr refuses to deserialize.
fn tags(raw: Vec<Vec<String>>) -> Option<Vec<Tag>> {
    raw.into_iter().map(|t| Tag::parse(t).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_ext::FinalizeUnsignedWithId;
    use nostr_sdk::prelude::{nip44, EventBuilder, FinalizeEvent, FinalizeUnsignedEvent, GiftWrapBuilder, Keys, ToBech32};

    const ID: &str = "70b10f70c1318967eddf12527799411b1a9780ad9c43858f5e5fcd45486a13a5";
    const PUBKEY: &str = "379e863e8357163b5bce5d2688dc4f1dcc2d505222fb8d74db600f30535dfdfe";
    const SIG: &str = "273a9cd5d11455590f4359500bccb7a89428262b96b3ea87a756b770964472f8c3e87f5d5e64d8d2e859a71462a3f477b554565c4f2f326cb01dd7620db71502";

    fn contents() -> Vec<String> {
        vec![
            String::new(),
            "hi".into(),
            "quotes \" and \\ backslashes, a tab\t, a newline\n and \u{1F680} \u{00e9}".into(),
            "\\u0041 stays literal; </script> too".into(),
            "long ".repeat(400),
        ]
    }

    fn rumors(me: &Keys, them: &Keys) -> Vec<UnsignedEvent> {
        let mut out = Vec::new();
        for (i, text) in contents().into_iter().enumerate() {
            let mut tags = vec![Tag::parse(["p", &them.public_key().to_hex()]).unwrap()];
            if i % 2 == 0 {
                tags.push(Tag::parse(["e", ID, "", "reply"]).unwrap());
                tags.push(Tag::parse(["emoji", "cat", "https://example.com/c.png?a=1&b=\"2\""]).unwrap());
            }
            let builder = EventBuilder::new(Kind::PrivateDirectMessage, text).tags(tags);
            out.push(builder.clone().finalize_unsigned_with_id(me.public_key()));
            out.push(builder.finalize_unsigned(me.public_key()));
        }
        out
    }

    /// The decrypted seals of gift wraps carrying `rumors`, plus the wraps themselves.
    fn seals_and_wraps(me: &Keys, them: &Keys) -> Vec<String> {
        let mut out = Vec::new();
        for rumor in rumors(me, them) {
            let wrap = GiftWrapBuilder::new(them.public_key(), rumor).finalize(me).unwrap();
            out.push(nip44::decrypt(them.secret_key(), &wrap.pubkey, &wrap.content).unwrap());
            out.push(wrap.as_json());
        }
        out
    }

    fn same_event(json: &str) {
        match (event(json), Event::from_json(json)) {
            (Ok(ours), Ok(theirs)) => {
                assert_eq!(ours, theirs, "{json}");
                assert_eq!(ours.as_json(), theirs.as_json(), "{json}");
            }
            (Err(_), Err(_)) => {}
            (ours, theirs) => panic!("ours {ours:?}, rust-nostr {theirs:?}: {json}"),
        }
    }

    fn same_unsigned(json: &str) {
        match (unsigned(json), UnsignedEvent::from_json(json)) {
            (Ok(ours), Ok(theirs)) => assert_eq!(ours, theirs, "{json}"),
            (Err(_), Err(_)) => {}
            (ours, theirs) => panic!("ours {ours:?}, rust-nostr {theirs:?}: {json}"),
        }
    }

    #[test]
    fn rumors_as_rust_nostr_writes_them_take_the_fast_path() {
        let (me, them) = (Keys::generate(), Keys::generate());
        for rumor in rumors(&me, &them) {
            let json = rumor.as_json();
            assert!(!json.contains("\"sig\""), "{json}");
            assert_eq!(fast_unsigned(&json).as_ref(), Some(&rumor), "{json}");
            same_unsigned(&json);
        }
    }

    #[test]
    fn seals_and_wraps_take_the_fast_path() {
        let (me, them) = (Keys::generate(), Keys::generate());
        for json in seals_and_wraps(&me, &them) {
            assert_eq!(fast_event(&json), Some(Event::from_json(&json).unwrap()), "{json}");
            same_event(&json);
        }
    }

    #[test]
    fn what_nostr_parse_refuses_still_reads_as_rust_nostr_reads_it() {
        let upper_id = format!(r#"{{"id":"{}","pubkey":"{PUBKEY}","created_at":1,"kind":14,"tags":[],"content":"x"}}"#, ID.to_uppercase());
        let npub = PublicKey::from_hex(PUBKEY).unwrap().to_bech32().unwrap();
        let bech32_key = format!(r#"{{"id":"{ID}","pubkey":"{npub}","created_at":1,"kind":14,"tags":[],"content":"x"}}"#);
        for json in [&upper_id, &bech32_key] {
            assert!(fast_unsigned(json).is_none(), "{json}");
            assert_eq!(unsigned(json).unwrap(), UnsignedEvent::from_json(json).unwrap(), "{json}");
        }

        let upper_sig = format!(r#"{{"id":"{ID}","pubkey":"{PUBKEY}","created_at":1,"kind":1,"tags":[],"content":"x","sig":"{}"}}"#, SIG.to_uppercase());
        assert!(fast_event(&upper_sig).is_none());
        assert_eq!(event(&upper_sig).unwrap(), Event::from_json(&upper_sig).unwrap());
    }

    #[test]
    fn rust_nostr_and_these_agree_on_every_spelling() {
        let base = |id: &str, extra: &str, tags: &str, sig: &str| {
            format!(r#"{{{id}"pubkey":"{PUBKEY}","created_at":1612809991,"kind":14,"tags":{tags},"content":"a \"b\" \\ c"{extra}{sig}}}"#)
        };
        let ids = [format!(r#""id":"{ID}","#), r#""id":null,"#.to_string(), String::new(), r#""id":"","#.to_string(), r#""id":7,"#.to_string()];
        let sigs = [String::new(), format!(r#","sig":"{SIG}""#), r#","sig":"""#.into(), r#","sig":null"#.into(), r#","sig":[1,{"a":2}]"#.into(), r#","sig":"","sig":"""#.into()];
        let extras = ["", r#","relays":["wss://x"]"#, r#","deep":[[[[{"a":[1,2,{"b":null}]}]]]]"#, r#","kind":1"#];
        let escaped = format!("[[\"{}u0070\",\"{}ud83d{}ude80\"]]", '\\', '\\', '\\');
        let tags = ["[]", r#"[["p","x"],["e","y","","reply"]]"#, "[[]]", r#"[["p",1]]"#, r#"[["p","🚀"]]"#, &escaped, "[\"p\"]"];
        let (mut n, mut fast_rumors, mut fast_events) = (0, 0, 0);
        for id in &ids {
            for sig in &sigs {
                for extra in extras {
                    for tag in tags {
                        let json = base(id, extra, tag, sig);
                        same_unsigned(&json);
                        same_event(&json);
                        same_unsigned(&format!(" \n{json}\t"));
                        fast_rumors += usize::from(fast_unsigned(&json).is_some());
                        fast_events += usize::from(fast_event(&json).is_some());
                        n += 1;
                    }
                }
            }
        }
        assert_eq!(n, ids.len() * sigs.len() * extras.len() * tags.len());
        // The sweep must reach the fast path, or agreement only proves the fallback.
        assert!(fast_rumors >= 100 && fast_events >= 10, "{fast_rumors} rumors, {fast_events} events");

        for json in ["", "{}", "[]", "null", "{\"id\":", "\u{feff}{}", r#"{"id":"ab"}"#] {
            same_unsigned(json);
            same_event(json);
        }
    }
}
