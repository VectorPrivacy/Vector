//! One side of a transfer as a pure state machine: messages in, messages and outcomes out. The
//! network loop feeds it; the tests drive two or three of them by hand.
//!
//! The joiner commits to its SPAKE2 message before it sees the shower's, and the shower sends its
//! own before it sees the joiner's, so a party that knows the code still can't steer the number
//! the two screens show. Any third device in the room ends the session. The identity itself travels
//! under SPAKE2's secret and an ML-KEM-1024 secret together: the receiver's encapsulation key rides
//! in its sealed hello, and the sender encapsulates to it when the user approves.

use nostr_sdk::prelude::{FromBech32, FromMnemonic, Keys, PublicKey, ToBech32};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use super::code::Code;
use super::crypto::{commitment, kem_encapsulate, kem_public_ok, room, transcript, Dir, KemSeed, Pake, Sealed, SessionKeys};
use super::wire::{DecodeError, Msg};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Signed in, holds the key.
    Sender,
    /// Being set up.
    Receiver,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Presentation {
    /// Displays the code.
    Shower,
    /// Scanned or typed it.
    Joiner,
}

/// The identity a sender hands over.
#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct Bundle {
    pub nsec: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
}

impl Bundle {
    /// The identity as stored on the sender. A seed that doesn't derive this key stays behind.
    pub fn from_stored(nsec: &str, seed: Option<&str>) -> Self {
        let pk = Keys::parse(nsec).ok().map(|k| k.public_key());
        let seed = seed.filter(|s| pk.is_some() && Keys::from_mnemonic(*s, None).ok().map(|k| k.public_key()) == pk);
        Self { nsec: nsec.to_string(), seed: seed.map(str::to_string) }
    }

    /// The key's public half, if the nsec parses and any seed derives the same key.
    fn identity(&self) -> Option<PublicKey> {
        let pk = Keys::parse(&self.nsec).ok()?.public_key();
        match &self.seed {
            Some(seed) => (Keys::from_mnemonic(seed.as_str(), None).ok()?.public_key() == pk).then_some(pk),
            None => Some(pk),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Failure {
    /// The two devices hold different codes.
    WrongCode,
    /// Another device used the code.
    Contention,
    /// Both devices are signed in, or neither is.
    RoleClash(Role),
    /// The other device speaks another version of the protocol.
    Version,
    /// The sender's user said no.
    Denied,
    /// The identity that arrived isn't the one the sender announced.
    BadBundle,
    /// The number was typed wrong too many times.
    WrongNumber,
    /// Out of order or malformed.
    Protocol(&'static str),
    /// The other device gave up.
    PeerAborted(String),
}

pub enum Out {
    Send(Msg),
    /// The peer repeated itself: our last message was lost, send it again.
    Resend,
    /// The keys agree. `sas` is shown on the receiver and typed on the sender; `sender` is the
    /// identity the sender announced, on the receiver.
    Matched { sas: String, sender: Option<PublicKey> },
    /// Receiver: the identity arrived and matches what was announced.
    Received(Bundle),
    /// Sender: the receiver has it.
    Acked,
    Failed(Failure),
}

impl std::fmt::Debug for Out {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Out::Send(m) => write!(f, "Send({})", m.kind()),
            Out::Resend => write!(f, "Resend"),
            Out::Matched { .. } => write!(f, "Matched"),
            Out::Received(_) => write!(f, "Received"),
            Out::Acked => write!(f, "Acked"),
            Out::Failed(why) => write!(f, "Failed({why:?})"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ApproveError {
    NotReady,
    WrongNumber { tries_left: u8 },
    NotYours,
}

#[derive(Serialize, Deserialize)]
struct Hello {
    role: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    npub: Option<String>,
    /// The receiver's ML-KEM-1024 encapsulation key, base64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kem: Option<String>,
    /// The sender's display name, for the new device's screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// The sender's avatar thumbnail, base64. Shown only after the receiver re-encodes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    avatar: Option<String>,
}

#[derive(PartialEq, Eq, Debug)]
enum Stage {
    AwaitCommit,
    AwaitPake,
    AwaitReveal([u8; 32]),
    AwaitHello,
    Matched,
    Sent,
    Done,
    Failed,
}

const SAS_TRIES: u8 = 3;

pub struct Session {
    role: Role,
    presentation: Presentation,
    me: PublicKey,
    room: PublicKey,
    identity: Option<PublicKey>,
    peer: Option<PublicKey>,
    pake: Option<Pake>,
    my_pake: Vec<u8>,
    keys: Option<SessionKeys>,
    announced: Option<PublicKey>,
    stage: Stage,
    seen: Vec<&'static str>,
    last: Option<Msg>,
    sas_checked: bool,
    sas_tries: u8,
    /// Receiver: its ML-KEM key. Sender: none.
    kem: Option<KemSeed>,
    my_kem_public: Vec<u8>,
    /// Sender: the receiver's encapsulation key, from its hello.
    peer_kem_public: Vec<u8>,
    /// The keys the bundle and its ack travel under, once the ML-KEM secret is in.
    hybrid: Option<SessionKeys>,
    /// Sender: its name and avatar thumbnail. Receiver: the sender's, from its hello.
    face_name: Option<String>,
    face_avatar: Option<Vec<u8>>,
}

impl Session {
    /// `me` is this device's throwaway author. A sender passes the identity it will hand over.
    pub fn new(role: Role, presentation: Presentation, code: &Code, me: PublicKey, identity: Option<PublicKey>) -> (Self, Vec<Out>) {
        let pake = Pake::start(code);
        let my_pake = pake.msg().to_vec();
        let kem = (role == Role::Receiver).then(KemSeed::generate);
        let my_kem_public = kem.as_ref().map(KemSeed::public_key).unwrap_or_default();
        let mut s = Self {
            role,
            presentation,
            me,
            room: room(code.nameplate()),
            identity: if role == Role::Sender { identity } else { None },
            peer: None,
            pake: Some(pake),
            my_pake,
            keys: None,
            announced: None,
            stage: if presentation == Presentation::Shower { Stage::AwaitCommit } else { Stage::AwaitPake },
            seen: Vec::new(),
            last: None,
            sas_checked: false,
            sas_tries: 0,
            kem,
            my_kem_public,
            peer_kem_public: Vec::new(),
            hybrid: None,
            face_name: None,
            face_avatar: None,
        };
        let out = match presentation {
            Presentation::Joiner => vec![s.send(Msg::Commit(commitment(&s.my_pake)))],
            Presentation::Shower => Vec::new(),
        };
        (s, out)
    }

    pub fn room(&self) -> PublicKey {
        self.room
    }

    /// Sender: the name and avatar thumbnail its hello carries, set before the hello goes out.
    pub fn set_face(&mut self, name: Option<String>, avatar: Option<Vec<u8>>) {
        if self.role == Role::Sender {
            self.face_name = name.map(|n| n.chars().take(64).collect());
            self.face_avatar = avatar.filter(|a| a.len() <= super::avatar::MAX_BYTES);
        }
    }

    /// Receiver: the name and avatar thumbnail the sender announced. Untrusted, for display only.
    pub fn sender_face(&self) -> (Option<&str>, Option<&[u8]>) {
        match self.role {
            Role::Receiver => (self.face_name.as_deref(), self.face_avatar.as_deref()),
            Role::Sender => (None, None),
        }
    }

    /// The message to send again when the peer seems to have missed it.
    pub fn last_sent(&self) -> Option<&Msg> {
        self.last.as_ref()
    }

    /// Finished one way or the other: nothing more will be sent or accepted.
    pub fn is_over(&self) -> bool {
        matches!(self.stage, Stage::Done | Stage::Failed)
    }

    /// Keys agreed and nothing handed over yet: the approval window.
    pub fn is_matched(&self) -> bool {
        self.stage == Stage::Matched
    }

    /// Sender: the identity went out and the ack hasn't come back yet.
    pub fn is_sent(&self) -> bool {
        self.stage == Stage::Sent
    }

    /// The peer message our last one answered: only a repeat of that means ours was lost. Anything
    /// else repeated is the peer retrying across our reply, and answering it would loop forever.
    fn answers(&self) -> Option<&'static str> {
        match self.last.as_ref()? {
            Msg::Pake(_) => Some("commit"),
            Msg::Reveal { .. } => Some("pake"),
            Msg::Hello(_) => Some("reveal"),
            Msg::Ack(_) => Some("bundle"),
            _ => None,
        }
    }

    fn send(&mut self, msg: Msg) -> Out {
        if !matches!(msg, Msg::Abort(_)) {
            self.last = Some(msg.clone());
        }
        Out::Send(msg)
    }

    fn fail(&mut self, failure: Failure) -> Vec<Out> {
        self.stage = Stage::Failed;
        vec![Out::Failed(failure)]
    }

    fn fail_loudly(&mut self, failure: Failure, reason: &str) -> Vec<Out> {
        self.stage = Stage::Failed;
        vec![Out::Send(Msg::Abort(reason.to_string())), Out::Failed(failure)]
    }

    fn my_dir(&self) -> Dir {
        match self.presentation {
            Presentation::Shower => Dir::ShowerToJoiner,
            Presentation::Joiner => Dir::JoinerToShower,
        }
    }

    fn peer_dir(&self) -> Dir {
        match self.presentation {
            Presentation::Shower => Dir::JoinerToShower,
            Presentation::Joiner => Dir::ShowerToJoiner,
        }
    }

    fn hello(&self) -> Zeroizing<Vec<u8>> {
        let kem = (!self.my_kem_public.is_empty()).then(|| base64_simd::STANDARD.encode_to_string(&self.my_kem_public));
        let (name, avatar) = match self.role {
            Role::Sender => (self.face_name.clone(), self.face_avatar.as_ref().map(|a| base64_simd::STANDARD.encode_to_string(a))),
            Role::Receiver => (None, None),
        };
        let hello = Hello { role: self.role, npub: self.identity.and_then(|pk| pk.to_bech32().ok()), kem, name, avatar };
        Zeroizing::new(serde_json::to_vec(&hello).expect("plain struct"))
    }

    /// Open the peer's hello and settle who is who.
    fn accept_hello(&mut self, sealed: &[u8]) -> Result<(), Vec<Out>> {
        let keys = self.keys.as_ref().expect("keys before hello");
        let Ok(plain) = keys.open(self.peer_dir(), Sealed::Hello, sealed) else {
            return Err(self.fail_loudly(Failure::WrongCode, "code"));
        };
        let Ok(hello) = serde_json::from_slice::<Hello>(&plain) else {
            return Err(self.fail_loudly(Failure::Protocol("malformed hello"), "protocol"));
        };
        if hello.role == self.role {
            return Err(self.fail_loudly(Failure::RoleClash(self.role), "role"));
        }
        if hello.role == Role::Sender {
            match hello.npub.as_deref().and_then(|n| PublicKey::from_bech32(n).ok()) {
                Some(pk) => self.announced = Some(pk),
                None => return Err(self.fail_loudly(Failure::Protocol("sender without identity"), "protocol")),
            }
            self.face_name = hello.name.map(|n| n.chars().take(64).collect());
            self.face_avatar = hello
                .avatar
                .and_then(|a| base64_simd::STANDARD.decode_to_vec(a).ok())
                .filter(|a| a.len() <= super::avatar::MAX_BYTES);
        } else {
            let kem = hello.kem.as_deref().and_then(|k| base64_simd::STANDARD.decode_to_vec(k).ok());
            match kem.filter(|k| kem_public_ok(k)) {
                Some(k) => self.peer_kem_public = k,
                None => return Err(self.fail_loudly(Failure::Protocol("receiver without a post-quantum key"), "protocol")),
            }
        }
        Ok(())
    }

    fn matched(&mut self) -> Out {
        self.stage = Stage::Matched;
        let sas = self.keys.as_ref().expect("matched with keys").sas();
        Out::Matched { sas, sender: self.announced }
    }

    /// Waiting on the peer's next message, so the last one should be repeated until it comes.
    pub fn awaiting_reply(&self) -> bool {
        matches!(self.stage, Stage::AwaitPake | Stage::AwaitReveal(_) | Stage::AwaitHello | Stage::Sent)
    }

    pub fn on_message(&mut self, author: PublicKey, msg: Result<Msg, DecodeError>) -> Vec<Out> {
        if author == self.me {
            return Vec::new();
        }
        // A receiver that has the identity still answers a repeated bundle, or the sender never
        // hears the ack.
        if self.stage == Stage::Done && self.peer == Some(author) {
            return match msg {
                Ok(m) if self.answers() == Some(m.kind()) => vec![Out::Resend],
                _ => Vec::new(),
            };
        }
        if self.is_over() {
            return Vec::new();
        }
        let msg = match msg {
            Ok(msg) => msg,
            Err(DecodeError::Version(_)) if self.peer.is_none_or(|p| p == author) => return self.fail(Failure::Version),
            Err(_) if self.peer == Some(author) => return self.fail(Failure::Protocol("malformed message")),
            Err(_) => return self.fail_loudly(Failure::Contention, "contention"),
        };

        match self.peer {
            Some(peer) if peer != author => return self.fail_loudly(Failure::Contention, "contention"),
            Some(_) => {}
            None => {
                let opens = matches!(
                    (&self.stage, &msg),
                    (Stage::AwaitCommit, Msg::Commit(_)) | (Stage::AwaitPake, Msg::Pake(_))
                );
                if !opens {
                    return self.fail_loudly(Failure::Contention, "contention");
                }
                self.peer = Some(author);
            }
        }

        if let Msg::Abort(reason) = msg {
            return self.fail(Failure::PeerAborted(reason));
        }
        let kind = msg.kind();
        if self.seen.contains(&kind) {
            return if self.answers() == Some(kind) { vec![Out::Resend] } else { Vec::new() };
        }
        self.seen.push(kind);

        match (std::mem::replace(&mut self.stage, Stage::Failed), msg) {
            (Stage::AwaitCommit, Msg::Commit(h)) => {
                self.stage = Stage::AwaitReveal(h);
                vec![self.send(Msg::Pake(self.my_pake.clone()))]
            }
            (Stage::AwaitPake, Msg::Pake(theirs)) => {
                let shared = match self.pake.take().expect("pake once").finish(&theirs) {
                    Ok(k) => k,
                    Err(why) => return self.fail_loudly(Failure::Protocol(why), "protocol"),
                };
                let t = transcript(&self.room, &author, &self.me, &theirs, &self.my_pake);
                let keys = SessionKeys::derive(&shared, t);
                let hello = keys.seal(self.my_dir(), Sealed::Hello, &self.hello());
                self.keys = Some(keys);
                self.stage = Stage::AwaitHello;
                vec![self.send(Msg::Reveal { pake: self.my_pake.clone(), hello })]
            }
            (Stage::AwaitReveal(h), Msg::Reveal { pake: theirs, hello }) => {
                if commitment(&theirs) != h {
                    return self.fail_loudly(Failure::Protocol("commitment mismatch"), "protocol");
                }
                let shared = match self.pake.take().expect("pake once").finish(&theirs) {
                    Ok(k) => k,
                    Err(why) => return self.fail_loudly(Failure::Protocol(why), "protocol"),
                };
                let t = transcript(&self.room, &self.me, &author, &self.my_pake, &theirs);
                self.keys = Some(SessionKeys::derive(&shared, t));
                if let Err(out) = self.accept_hello(&hello) {
                    return out;
                }
                let mine = self.keys.as_ref().expect("derived").seal(self.my_dir(), Sealed::Hello, &self.hello());
                let send = self.send(Msg::Hello(mine));
                vec![send, self.matched()]
            }
            (Stage::AwaitHello, Msg::Hello(sealed)) => {
                if let Err(out) = self.accept_hello(&sealed) {
                    return out;
                }
                vec![self.matched()]
            }
            (Stage::Matched, Msg::Bundle { kem, sealed }) if self.role == Role::Receiver => {
                let keys = self.keys.as_ref().expect("matched with keys");
                let Ok(shared) = self.kem.as_ref().expect("receiver has a kem key").decapsulate(&kem) else {
                    return self.fail_loudly(Failure::Protocol("malformed kem ciphertext"), "protocol");
                };
                let hybrid = keys.hybrid(&self.my_kem_public, &kem, &shared);
                let Ok(plain) = hybrid.open(self.peer_dir(), Sealed::Bundle, &sealed) else {
                    return self.fail_loudly(Failure::Protocol("unreadable bundle"), "protocol");
                };
                let Ok(bundle) = serde_json::from_slice::<Bundle>(&plain) else {
                    return self.fail_loudly(Failure::BadBundle, "bundle");
                };
                if bundle.identity().is_none() || bundle.identity() != self.announced {
                    return self.fail_loudly(Failure::BadBundle, "bundle");
                }
                let ack = hybrid.seal(self.my_dir(), Sealed::Ack, b"");
                self.hybrid = Some(hybrid);
                self.stage = Stage::Done;
                // The account is ready before the ack goes out: a slow publish mustn't hold up sign-in.
                let send = self.send(Msg::Ack(ack));
                vec![Out::Received(bundle), send]
            }
            (Stage::Matched, Msg::Deny(sealed)) if self.role == Role::Receiver => {
                let keys = self.keys.as_ref().expect("matched with keys");
                if keys.open(self.peer_dir(), Sealed::Deny, &sealed).is_err() {
                    return self.fail(Failure::Protocol("unreadable deny"));
                }
                self.fail(Failure::Denied)
            }
            (Stage::Sent, Msg::Ack(sealed)) => {
                let keys = self.hybrid.as_ref().expect("sent under hybrid keys");
                if keys.open(self.peer_dir(), Sealed::Ack, &sealed).is_err() {
                    return self.fail(Failure::Protocol("unreadable ack"));
                }
                self.stage = Stage::Done;
                vec![Out::Acked]
            }
            (stage, _) => {
                self.stage = stage;
                self.fail_loudly(Failure::Protocol("unexpected message"), "protocol")
            }
        }
    }

    /// Sender: check the number the user typed from the new device. Three wrong tries end it.
    pub fn check_number(&mut self, typed: &str) -> Result<(), ApproveError> {
        if self.role != Role::Sender || self.stage != Stage::Matched {
            return Err(ApproveError::NotReady);
        }
        if self.keys.as_ref().expect("matched with keys").sas_matches(typed) {
            self.sas_checked = true;
            return Ok(());
        }
        self.sas_tries += 1;
        if self.sas_tries >= SAS_TRIES {
            self.stage = Stage::Failed;
        }
        Err(ApproveError::WrongNumber { tries_left: SAS_TRIES.saturating_sub(self.sas_tries) })
    }

    /// Sender: hand the identity over. Once only, and only after the number checked out.
    pub fn approve(&mut self, bundle: Bundle) -> Result<Vec<Out>, ApproveError> {
        if self.role != Role::Sender || self.stage != Stage::Matched || !self.sas_checked {
            return Err(ApproveError::NotReady);
        }
        if bundle.identity().is_none() || bundle.identity() != self.identity {
            return Err(ApproveError::NotYours);
        }
        let (kem, shared) = kem_encapsulate(&self.peer_kem_public).map_err(|_| ApproveError::NotReady)?;
        let hybrid = self.keys.as_ref().expect("matched with keys").hybrid(&self.peer_kem_public, &kem, &shared);
        // Sized up front so serializing never reallocates and strands an unwiped copy of the key.
        let mut plain = Zeroizing::new(Vec::with_capacity(1024));
        serde_json::to_writer(&mut *plain, &bundle).expect("plain struct");
        let sealed = hybrid.seal(self.my_dir(), Sealed::Bundle, &plain);
        self.hybrid = Some(hybrid);
        self.stage = Stage::Sent;
        Ok(vec![self.send(Msg::Bundle { kem, sealed })])
    }

    /// Sender: the user said no.
    pub fn deny(&mut self) -> Vec<Out> {
        if self.role != Role::Sender || self.stage != Stage::Matched {
            return Vec::new();
        }
        let sealed = self.keys.as_ref().expect("matched with keys").seal(self.my_dir(), Sealed::Deny, b"");
        self.stage = Stage::Done;
        vec![self.send(Msg::Deny(sealed))]
    }

    /// The host is giving up (cancel, timeout): tell the peer so it doesn't wait.
    pub fn abort(&mut self, reason: &str) -> Vec<Out> {
        if self.is_over() {
            return Vec::new();
        }
        self.stage = Stage::Failed;
        vec![Out::Send(Msg::Abort(reason.to_string()))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Device {
        session: Session,
        author: PublicKey,
        outbox: Vec<Msg>,
        events: Vec<Out>,
    }

    impl Device {
        fn new(role: Role, presentation: Presentation, code: &str, identity: Option<&Keys>) -> Self {
            let author = Keys::generate().public_key();
            let (session, out) = Session::new(role, presentation, &Code::parse(code).unwrap(), author, identity.map(|k| k.public_key()));
            let mut d = Self { session, author, outbox: Vec::new(), events: Vec::new() };
            d.absorb(out);
            d
        }

        fn absorb(&mut self, out: Vec<Out>) {
            for o in out {
                match o {
                    Out::Send(m) => self.outbox.push(m),
                    Out::Resend => {
                        if let Some(m) = self.session.last_sent().cloned() {
                            self.outbox.push(m);
                        }
                    }
                    other => self.events.push(other),
                }
            }
        }

        fn take(&mut self) -> Vec<Msg> {
            std::mem::take(&mut self.outbox)
        }

        fn hear(&mut self, from: PublicKey, msgs: Vec<Msg>) {
            for m in msgs {
                let encoded = m.encode();
                let out = self.session.on_message(from, Msg::decode(&encoded));
                self.absorb(out);
            }
        }

        fn sas(&self) -> Option<String> {
            self.events.iter().find_map(|e| match e {
                Out::Matched { sas, .. } => Some(sas.clone()),
                _ => None,
            })
        }

        fn failure(&self) -> Option<Failure> {
            self.events.iter().find_map(|e| match e {
                Out::Failed(f) => Some(f.clone()),
                _ => None,
            })
        }
    }

    /// Deliver everything both ways until nobody has anything left to say.
    fn pump(a: &mut Device, b: &mut Device) {
        for _ in 0..10 {
            let (to_b, to_a) = (a.take(), b.take());
            if to_b.is_empty() && to_a.is_empty() {
                return;
            }
            b.hear(a.author, to_b);
            a.hear(b.author, to_a);
        }
        panic!("the devices kept talking: a resend loop");
    }

    fn bundle_for(keys: &Keys, seed: Option<&str>) -> Bundle {
        Bundle { nsec: keys.secret_key().to_bech32().unwrap(), seed: seed.map(str::to_string) }
    }

    const CODE: &str = "7-orbit-lemon-stage";

    fn matched_pair(sender_shows: bool) -> (Device, Device, Keys) {
        let identity = Keys::generate();
        let (sp, rp) = if sender_shows { (Presentation::Shower, Presentation::Joiner) } else { (Presentation::Joiner, Presentation::Shower) };
        let mut sender = Device::new(Role::Sender, sp, CODE, Some(&identity));
        let mut receiver = Device::new(Role::Receiver, rp, CODE, None);
        pump(&mut sender, &mut receiver);
        (sender, receiver, identity)
    }

    #[test]
    fn the_identity_moves_whichever_device_shows_the_code() {
        for sender_shows in [true, false] {
            let (mut sender, mut receiver, identity) = matched_pair(sender_shows);
            let sas = receiver.sas().expect("receiver matched");
            assert_eq!(sender.sas().as_deref(), Some(sas.as_str()));
            assert!(receiver.events.iter().any(|e| matches!(e, Out::Matched { sender: Some(pk), .. } if *pk == identity.public_key())));

            sender.session.check_number(&sas).unwrap();
            let out = sender.session.approve(bundle_for(&identity, None)).unwrap();
            sender.absorb(out);
            pump(&mut sender, &mut receiver);

            let received = receiver.events.iter().find_map(|e| match e { Out::Received(b) => Some(b.nsec.clone()), _ => None });
            assert_eq!(received.as_deref(), Some(bundle_for(&identity, None).nsec.as_str()));
            assert!(sender.events.iter().any(|e| matches!(e, Out::Acked)));
            assert!(sender.session.is_over() && receiver.session.is_over());

            // The ack was lost: the repeated bundle gets it again, and nothing else changes.
            let bundle = sender.session.last_sent().cloned().unwrap();
            receiver.hear(sender.author, vec![bundle]);
            assert!(matches!(receiver.take().as_slice(), [Msg::Ack(_)]));
            assert_eq!(receiver.events.iter().filter(|e| matches!(e, Out::Received(_))).count(), 1);
        }
    }

    #[test]
    fn the_senders_face_reaches_the_receiver_and_only_the_receiver() {
        let identity = Keys::generate();
        let mut sender = Device::new(Role::Sender, Presentation::Joiner, CODE, Some(&identity));
        sender.session.set_face(Some("Kitty".repeat(20)), Some(vec![0xff, 0xd8, 0xff, 1, 2, 3]));
        let mut receiver = Device::new(Role::Receiver, Presentation::Shower, CODE, None);
        receiver.session.set_face(Some("ignored".into()), None);
        pump(&mut sender, &mut receiver);
        let (name, avatar) = receiver.session.sender_face();
        assert_eq!(name.map(|n| n.chars().count()), Some(64), "names are capped");
        assert_eq!(avatar, Some(&[0xff, 0xd8, 0xff, 1, 2, 3][..]));
        assert_eq!(sender.session.sender_face(), (None, None));
    }

    #[test]
    fn a_seed_travels_only_with_the_key_it_derives() {
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let from_seed = Keys::from_mnemonic(phrase, None).unwrap();
        let mut sender = Device::new(Role::Sender, Presentation::Shower, CODE, Some(&from_seed));
        let mut receiver = Device::new(Role::Receiver, Presentation::Joiner, CODE, None);
        pump(&mut sender, &mut receiver);
        sender.session.check_number(&receiver.sas().unwrap()).unwrap();
        assert_eq!(sender.session.approve(bundle_for(&Keys::generate(), Some(phrase))).unwrap_err(), ApproveError::NotYours);
        let stray = Bundle::from_stored(&bundle_for(&Keys::generate(), None).nsec, Some(phrase));
        assert!(stray.seed.is_none(), "a seed for another key is left behind");
        let out = sender.session.approve(bundle_for(&from_seed, Some(phrase))).unwrap();
        sender.absorb(out);
        pump(&mut sender, &mut receiver);
        assert!(receiver.events.iter().any(|e| matches!(e, Out::Received(b) if b.seed.as_deref() == Some(phrase))));
    }

    #[test]
    fn a_different_code_ends_both_sides() {
        let mut shower = Device::new(Role::Receiver, Presentation::Shower, CODE, None);
        let mut joiner = Device::new(Role::Sender, Presentation::Joiner, "7-orbit-lemon-stamp", Some(&Keys::generate()));
        pump(&mut shower, &mut joiner);
        assert_eq!(shower.failure(), Some(Failure::WrongCode));
        assert!(matches!(joiner.failure(), Some(Failure::PeerAborted(_))));
        assert!(shower.sas().is_none() && joiner.sas().is_none());
    }

    #[test]
    fn a_third_device_in_the_room_ends_the_session() {
        let identity = Keys::generate();
        let mut shower = Device::new(Role::Sender, Presentation::Shower, CODE, Some(&identity));
        let mut real = Device::new(Role::Receiver, Presentation::Joiner, CODE, None);
        let mut intruder = Device::new(Role::Receiver, Presentation::Joiner, CODE, None);
        shower.hear(intruder.author, intruder.take());
        shower.hear(real.author, real.take());
        assert_eq!(shower.failure(), Some(Failure::Contention));
        shower.take();
        assert_eq!(shower.session.check_number("000000"), Err(ApproveError::NotReady));

        // After the keys agree, too.
        let (mut sender, mut receiver, _) = matched_pair(true);
        let late = Keys::generate().public_key();
        sender.hear(late, vec![Msg::Commit([0; 32])]);
        assert_eq!(sender.failure(), Some(Failure::Contention));
        assert_eq!(sender.session.check_number(&receiver.sas().unwrap()), Err(ApproveError::NotReady));
        receiver.hear(late, vec![Msg::Abort("x".into())]);
        assert_eq!(receiver.failure(), Some(Failure::Contention));
    }

    #[test]
    fn a_joiner_cannot_reveal_other_than_it_committed() {
        let mut shower = Device::new(Role::Sender, Presentation::Shower, CODE, Some(&Keys::generate()));
        let mut joiner = Device::new(Role::Receiver, Presentation::Joiner, CODE, None);
        shower.hear(joiner.author, joiner.take());
        joiner.hear(shower.author, shower.take());
        let other = Pake::start(&Code::parse(CODE).unwrap());
        let swapped: Vec<Msg> = joiner.take().into_iter().map(|m| match m {
            Msg::Reveal { hello, .. } => Msg::Reveal { pake: other.msg().to_vec(), hello },
            m => m,
        }).collect();
        shower.hear(joiner.author, swapped);
        assert_eq!(shower.failure(), Some(Failure::Protocol("commitment mismatch")));
    }

    #[test]
    fn the_showers_own_message_reflected_back_is_refused() {
        let mut shower = Device::new(Role::Sender, Presentation::Shower, CODE, Some(&Keys::generate()));
        let mallory = Keys::generate().public_key();
        let own = shower.session.my_pake.clone();
        shower.hear(mallory, vec![Msg::Commit(commitment(&own))]);
        shower.take();
        shower.hear(mallory, vec![Msg::Reveal { pake: own, hello: vec![0; 32] }]);
        assert_eq!(shower.failure(), Some(Failure::Protocol("reflected pake message")));
    }

    #[test]
    fn two_signed_in_devices_or_two_new_ones_clash() {
        for role in [Role::Sender, Role::Receiver] {
            let id = (role == Role::Sender).then(Keys::generate);
            let mut a = Device::new(role, Presentation::Shower, CODE, id.as_ref());
            let mut b = Device::new(role, Presentation::Joiner, CODE, id.as_ref());
            pump(&mut a, &mut b);
            assert_eq!(a.failure(), Some(Failure::RoleClash(role)));
            assert!(a.sas().is_none() && b.sas().is_none());
        }
    }

    #[test]
    fn the_number_gates_approval_and_approval_happens_once() {
        let (mut sender, receiver, identity) = matched_pair(false);
        let sas = receiver.sas().unwrap();
        assert_eq!(sender.session.approve(bundle_for(&identity, None)).unwrap_err(), ApproveError::NotReady, "no number yet");
        assert_eq!(sender.session.check_number("000000x"), Err(ApproveError::WrongNumber { tries_left: 2 }));
        sender.session.check_number(&sas).unwrap();
        assert!(sender.session.approve(bundle_for(&identity, None)).is_ok());
        assert_eq!(sender.session.approve(bundle_for(&identity, None)).unwrap_err(), ApproveError::NotReady, "once");
        assert!(sender.session.deny().is_empty(), "no deny after a bundle");

        let (mut sender, _, identity) = matched_pair(true);
        for left in [2, 1, 0] {
            assert_eq!(sender.session.check_number("not it"), Err(ApproveError::WrongNumber { tries_left: left }));
        }
        assert!(sender.session.is_over());
        assert_eq!(sender.session.approve(bundle_for(&identity, None)).unwrap_err(), ApproveError::NotReady);
    }

    #[test]
    fn a_denial_reaches_the_receiver_and_nothing_follows_it() {
        let (mut sender, mut receiver, identity) = matched_pair(true);
        let out = sender.session.deny();
        sender.absorb(out);
        pump(&mut sender, &mut receiver);
        assert_eq!(receiver.failure(), Some(Failure::Denied));
        assert!(sender.session.check_number(&receiver.sas().unwrap()).is_err());
        assert!(sender.session.approve(bundle_for(&identity, None)).is_err());
    }

    /// A bundle sealed the way an honest sender would, but carrying `bundle`.
    fn forge(sender: &Device, receiver: &Device, bundle: &Bundle) -> Msg {
        let pk = &receiver.session.my_kem_public;
        let (kem, shared) = kem_encapsulate(pk).unwrap();
        let hybrid = sender.session.keys.as_ref().unwrap().hybrid(pk, &kem, &shared);
        let sealed = hybrid.seal(sender.session.my_dir(), Sealed::Bundle, &serde_json::to_vec(bundle).unwrap());
        Msg::Bundle { kem, sealed }
    }

    #[test]
    fn a_bundle_for_another_identity_is_refused() {
        let (sender, mut receiver, _) = matched_pair(true);
        let forged = forge(&sender, &receiver, &bundle_for(&Keys::generate(), None));
        receiver.hear(sender.author, vec![forged]);
        assert_eq!(receiver.failure(), Some(Failure::BadBundle));
        assert!(!receiver.events.iter().any(|e| matches!(e, Out::Received(_))));
    }

    #[test]
    fn the_identity_needs_the_post_quantum_secret_as_well() {
        let (mut sender, receiver, identity) = matched_pair(true);
        sender.session.check_number(&receiver.sas().unwrap()).unwrap();
        let out = sender.session.approve(bundle_for(&identity, None)).unwrap();
        let Some(Out::Send(Msg::Bundle { kem, sealed })) = out.into_iter().next() else { panic!("a bundle") };
        // Someone who later recovers SPAKE2's keys (a quantum computer replaying a recording) has
        // the classical keys and the ciphertext, but not the receiver's ML-KEM secret.
        let classical = sender.session.keys.as_ref().unwrap();
        assert!(classical.open(Dir::ShowerToJoiner, Sealed::Bundle, &sealed).is_err());
        let wrong_kem = KemSeed::generate().decapsulate(&kem).unwrap();
        let guessed = classical.hybrid(&receiver.session.my_kem_public, &kem, &wrong_kem);
        assert!(guessed.open(Dir::ShowerToJoiner, Sealed::Bundle, &sealed).is_err());
    }

    #[test]
    fn a_bent_kem_ciphertext_delivers_nothing() {
        let (mut sender, mut receiver, identity) = matched_pair(false);
        sender.session.check_number(&receiver.sas().unwrap()).unwrap();
        let out = sender.session.approve(bundle_for(&identity, None)).unwrap();
        let Some(Out::Send(Msg::Bundle { mut kem, sealed })) = out.into_iter().next() else { panic!("a bundle") };
        kem[100] ^= 0x40;
        receiver.hear(sender.author, vec![Msg::Bundle { kem, sealed }]);
        assert_eq!(receiver.failure(), Some(Failure::Protocol("unreadable bundle")));
        assert!(!receiver.events.iter().any(|e| matches!(e, Out::Received(_))));
    }

    #[test]
    fn a_receiver_without_a_sound_post_quantum_key_is_refused() {
        // Missing, and present but outside the field FIPS 203 allows.
        for bad in [Vec::new(), vec![0xff; super::super::crypto::KEM_PUBLIC_LEN]] {
            let identity = Keys::generate();
            let mut sender = Device::new(Role::Sender, Presentation::Shower, CODE, Some(&identity));
            let mut receiver = Device::new(Role::Receiver, Presentation::Joiner, CODE, None);
            receiver.session.my_kem_public = bad;
            pump(&mut sender, &mut receiver);
            assert_eq!(sender.failure(), Some(Failure::Protocol("receiver without a post-quantum key")));
            assert!(sender.sas().is_none());
        }
    }

    #[test]
    fn a_repeated_message_asks_for_a_resend_rather_than_restarting() {
        let mut shower = Device::new(Role::Receiver, Presentation::Shower, CODE, None);
        let mut joiner = Device::new(Role::Sender, Presentation::Joiner, CODE, Some(&Keys::generate()));
        let commit = joiner.take();
        shower.hear(joiner.author, commit.clone());
        let first = shower.take();
        shower.hear(joiner.author, commit);
        assert_eq!(shower.take(), first, "the same Pake again");
        assert!(shower.failure().is_none());
    }

    #[test]
    fn a_retransmit_that_crosses_a_reply_settles_instead_of_looping() {
        // The joiner's timer repeats its Reveal while the shower's Hello is still in flight.
        let mut shower = Device::new(Role::Receiver, Presentation::Shower, CODE, None);
        let mut joiner = Device::new(Role::Sender, Presentation::Joiner, CODE, Some(&Keys::generate()));
        shower.hear(joiner.author, joiner.take());
        joiner.hear(shower.author, shower.take());
        let reveal = joiner.take();
        shower.hear(joiner.author, reveal.clone());
        let hello = shower.take();
        shower.hear(joiner.author, reveal);
        let repeated_hello = shower.take();
        assert_eq!(repeated_hello.len(), 1, "a lost Hello is sent again");
        joiner.hear(shower.author, hello);
        joiner.hear(shower.author, repeated_hello);
        assert!(joiner.take().is_empty(), "the joiner doesn't answer a repeated Hello with a Reveal");
        assert!(joiner.sas().is_some() && shower.sas().is_some());
        pump(&mut shower, &mut joiner);
    }

    #[test]
    fn a_sender_waiting_on_its_ack_ignores_stale_handshake_repeats() {
        let (mut sender, receiver, identity) = matched_pair(false);
        sender.session.check_number(&receiver.sas().unwrap()).unwrap();
        let out = sender.session.approve(bundle_for(&identity, None)).unwrap();
        sender.absorb(out);
        sender.take();
        // The receiver's earlier messages turn up again: none of them is answered with the bundle.
        sender.hear(receiver.author, vec![Msg::Pake(vec![0x53; 33]), Msg::Hello(vec![1; 40])]);
        assert!(sender.take().is_empty());
        assert!(sender.failure().is_none() && sender.session.is_sent());
    }

    #[test]
    fn another_protocol_version_says_so() {
        let mut shower = Device::new(Role::Receiver, Presentation::Shower, CODE, None);
        let out = shower.session.on_message(Keys::generate().public_key(), Msg::decode(r#"{"v":9,"t":"commit"}"#));
        shower.absorb(out);
        assert_eq!(shower.failure(), Some(Failure::Version));
    }

    #[test]
    fn a_relay_that_splices_in_its_own_shower_cannot_complete() {
        // An attacker who knows the code answers the joiner first, then the real shower answers
        // too: the joiner sees two showers and stops, before any number is shown.
        let mut joiner = Device::new(Role::Receiver, Presentation::Joiner, CODE, None);
        let mut fake = Device::new(Role::Sender, Presentation::Shower, CODE, Some(&Keys::generate()));
        let mut real = Device::new(Role::Sender, Presentation::Shower, CODE, Some(&Keys::generate()));
        let commit = joiner.take();
        fake.hear(joiner.author, commit.clone());
        real.hear(joiner.author, commit);
        joiner.hear(fake.author, fake.take());
        joiner.hear(real.author, real.take());
        assert_eq!(joiner.failure(), Some(Failure::Contention));
        assert!(joiner.sas().is_none());
    }
}
