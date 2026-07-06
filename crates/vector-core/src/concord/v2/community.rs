//! The in-memory Community: keys held, channels known, and the founding /
//! joining flows that mint them (CORD-02).
//!
//! This is the orchestration point over the pure pieces: `found` mints the
//! genesis (exactly two owner-signed editions — metadata and `#general`,
//! nothing more), `join` turns a validated bundle into held keys, and the
//! address helpers answer "where does this Community live on relays" for a
//! transport.

use nostr_sdk::prelude::*;
use rand::RngCore;

use super::control::{ChannelMetadata, CommunityMetadata, ControlFold};
use super::derive::{self, GroupKey};
use super::edition::build_edition_rumor;
use super::invite::{ChannelGrant, CommunityInvite, InviteError};
use super::stream::{build_rumor, TAG_MS};
use super::{
    kind, split_ms, vsk, ChannelId, ChannelKey, CommunityId, CommunityRoot, Epoch, OwnerSalt,
};

/// One Channel as held locally.
#[derive(Debug, Clone)]
pub struct Channel {
    pub id: ChannelId,
    pub name: String,
    pub private: bool,
    pub deleted: bool,
    /// A Private Channel's independent secret; `None` for Public Channels
    /// (their key derives from the root) and for Private ones not granted.
    pub key: Option<ChannelKey>,
    /// A Private Channel's own epoch; Public Channels ride the root's.
    pub epoch: Epoch,
}

/// A Community as held locally: identity, access keys, channels.
#[derive(Debug, Clone)]
pub struct Community {
    pub id: CommunityId,
    pub owner: PublicKey,
    pub owner_salt: OwnerSalt,
    pub root: CommunityRoot,
    pub root_epoch: Epoch,
    pub name: String,
    pub relays: Vec<String>,
    pub channels: Vec<Channel>,
}

fn random_32() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes
}

/// A freshly founded Community plus its genesis edition rumors, ready to be
/// plaintext-sealed and wrapped at the Control Plane address.
pub struct Founded {
    pub community: Community,
    /// Exactly two owner-signed editions: the metadata and one public
    /// `#general` — no default roles, no scaffolding.
    pub genesis: Vec<UnsignedEvent>,
}

impl Community {
    /// Found a Community (CORD-02 §1): mint the salt (identity), the root
    /// (access), and `#general`.
    pub fn found(owner: PublicKey, name: &str, relays: Vec<String>, unix_ms: u64) -> Result<Founded, String> {
        let metadata = CommunityMetadata {
            name: name.to_string(),
            description: None,
            relays: relays.clone(),
            icon: None,
            banner: None,
            custom: None,
            extra: Default::default(),
        };
        metadata.validate()?;

        let owner_salt = OwnerSalt(random_32());
        let id = derive::community_id(&owner.to_bytes(), &owner_salt);
        let root = CommunityRoot(random_32());
        let general = ChannelId(random_32());

        let community = Community {
            id,
            owner,
            owner_salt,
            root,
            root_epoch: Epoch(0),
            name: name.to_string(),
            relays,
            channels: vec![Channel {
                id: general,
                name: "general".into(),
                private: false,
                deleted: false,
                key: None,
                epoch: Epoch(0),
            }],
        };

        let (secs, _) = split_ms(unix_ms);
        let meta_content = serde_json::to_string(&metadata).map_err(|e| e.to_string())?;
        let general_meta = ChannelMetadata {
            name: "general".into(),
            private: false,
            deleted: false,
            custom: None,
            extra: Default::default(),
        };
        let general_content = serde_json::to_string(&general_meta).map_err(|e| e.to_string())?;
        let genesis = vec![
            build_edition_rumor(owner, vsk::COMMUNITY_METADATA, &id.0, 1, None, &meta_content, secs, None),
            build_edition_rumor(owner, vsk::CHANNEL_METADATA, &general.0, 1, None, &general_content, secs, None),
        ];

        Ok(Founded { community, genesis })
    }

    /// Join from a bundle: validate (self-certifying owner, bounds) and
    /// refuse an expired one — the keys ARE membership, this just holds them.
    pub fn join(mut bundle: CommunityInvite, now_ms: u64) -> Result<Community, InviteError> {
        bundle.validate()?;
        if bundle.is_expired(now_ms) {
            return Err(InviteError::Expired);
        }
        let id = bundle
            .community_id_typed()
            .ok_or_else(|| InviteError::Malformed("community_id".into()))?;
        let owner_bytes = crate::simd::hex::hex_to_bytes_32_checked(&bundle.owner)
            .ok_or_else(|| InviteError::Malformed("owner".into()))?;
        let owner = PublicKey::from_slice(&owner_bytes).map_err(|e| InviteError::Malformed(e.to_string()))?;
        let owner_salt = crate::simd::hex::hex_to_bytes_32_checked(&bundle.owner_salt)
            .map(OwnerSalt)
            .ok_or_else(|| InviteError::Malformed("owner_salt".into()))?;
        let root = bundle
            .community_root_typed()
            .ok_or_else(|| InviteError::Malformed("community_root".into()))?;

        let channels = bundle
            .channels
            .iter()
            .filter_map(|grant: &ChannelGrant| {
                Some(Channel {
                    id: grant.channel_id()?,
                    name: grant.name.clone(),
                    private: grant.key.is_some(),
                    deleted: false,
                    key: grant.channel_key(),
                    epoch: Epoch(grant.epoch),
                })
            })
            .collect();

        Ok(Community {
            id,
            owner,
            owner_salt,
            root,
            root_epoch: Epoch(bundle.root_epoch),
            name: bundle.name.clone(),
            relays: bundle.relays.clone(),
            channels,
        })
    }

    /// Mint an invite bundle granting the given channels (Private ones carry
    /// their keys; Public ones ride the root).
    pub fn invite_bundle(
        &self,
        grant_channels: &[ChannelId],
        expires_at: Option<u64>,
        creator_npub: Option<String>,
        label: Option<String>,
    ) -> CommunityInvite {
        let channels = self
            .channels
            .iter()
            .filter(|c| !c.deleted && grant_channels.contains(&c.id))
            .map(|c| ChannelGrant {
                id: c.id.to_hex(),
                key: c.key.as_ref().map(|k| crate::simd::hex::bytes_to_hex_32(k.as_bytes())),
                epoch: if c.private { c.epoch.0 } else { self.root_epoch.0 },
                name: c.name.clone(),
            })
            .collect();
        CommunityInvite {
            community_id: self.id.to_hex(),
            owner: self.owner.to_hex(),
            owner_salt: crate::simd::hex::bytes_to_hex_32(&self.owner_salt.0),
            community_root: crate::simd::hex::bytes_to_hex_32(self.root.as_bytes()),
            root_epoch: self.root_epoch.0,
            channels,
            relays: self.relays.clone(),
            name: self.name.clone(),
            icon: None,
            expires_at,
            creator_npub,
            label,
            extra: Default::default(),
        }
    }

    // --- plane addresses ---

    pub fn control_key(&self) -> GroupKey {
        derive::control_key(&self.root, &self.id, self.root_epoch)
    }

    pub fn guestbook_key(&self) -> GroupKey {
        derive::guestbook_key(&self.root, &self.id, self.root_epoch)
    }

    pub fn dissolved_key(&self) -> GroupKey {
        derive::dissolved_address(&self.id)
    }

    pub fn channel(&self, id: &ChannelId) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == *id)
    }

    /// A Channel's group key at its current epoch, `None` for a Private
    /// Channel whose key we don't hold.
    pub fn channel_key(&self, id: &ChannelId) -> Option<GroupKey> {
        let chan = self.channel(id)?;
        if chan.private {
            Some(derive::private_channel_key(chan.key.as_ref()?, &chan.id, chan.epoch))
        } else {
            Some(derive::public_channel_key(&self.root, &chan.id, self.root_epoch))
        }
    }

    /// A Channel's effective epoch: its own for Private, the root's for Public.
    pub fn channel_epoch(&self, id: &ChannelId) -> Option<Epoch> {
        let chan = self.channel(id)?;
        Some(if chan.private { chan.epoch } else { self.root_epoch })
    }

    /// The precomputed next-rekey subscription addresses (CORD-06 §2): per
    /// held Private Channel the NEXT channel-epoch's rekey address, plus the
    /// next base-rotation address.
    pub fn rekey_subscription_authors(&self) -> Vec<PublicKey> {
        let mut authors: Vec<PublicKey> = self
            .channels
            .iter()
            .filter(|c| c.private && c.key.is_some() && !c.deleted)
            .map(|c| derive::rekey_address(&self.root, &c.id, Epoch(c.epoch.0 + 1)).public_key())
            .collect();
        authors.push(
            derive::base_rekey_address(&self.root, &self.id, Epoch(self.root_epoch.0 + 1)).public_key(),
        );
        authors
    }

    // --- key rotation application (CORD-06) ---

    /// Adopt a new base root at its epoch (a Refounding's outcome for a
    /// surviving member).
    pub fn apply_new_root(&mut self, new_root: CommunityRoot, new_epoch: Epoch) {
        if new_epoch > self.root_epoch {
            self.root = new_root;
            self.root_epoch = new_epoch;
        }
    }

    /// Adopt a Private Channel's new key at its epoch.
    pub fn apply_channel_key(&mut self, id: &ChannelId, key: ChannelKey, epoch: Epoch) {
        if let Some(chan) = self.channels.iter_mut().find(|c| c.id == *id) {
            if epoch > chan.epoch || chan.key.is_none() {
                chan.key = Some(key);
                chan.epoch = epoch;
                chan.private = true;
            }
        }
    }

    /// Refresh presentation + channel definitions from the folded Control
    /// Plane (the fold is always the authority; the bundle copy was a
    /// join-time snapshot). Held keys are never touched.
    pub fn apply_control(&mut self, fold: &ControlFold) {
        if let Some(meta) = fold.metadata() {
            self.name = meta.name.clone();
            if !meta.relays.is_empty() {
                self.relays = meta.effective_relays().to_vec();
            }
        }
        for (id, meta) in fold.channels() {
            match self.channels.iter_mut().find(|c| c.id == id) {
                Some(chan) => {
                    chan.name = meta.name.clone();
                    chan.deleted = meta.deleted;
                    chan.private = meta.private;
                }
                None => self.channels.push(Channel {
                    id,
                    name: meta.name.clone(),
                    private: meta.private,
                    deleted: meta.deleted,
                    key: None,
                    epoch: Epoch(0),
                }),
            }
        }
    }
}

// ============================================================================
// Chat rumor builders (CORD-03 §3, shapes per the registry)
// ============================================================================

/// A kind 9 message (NIP-C7 shape), channel/epoch-bound.
pub fn build_message(author: PublicKey, channel: &ChannelId, epoch: Epoch, text: &str, unix_ms: u64) -> UnsignedEvent {
    build_rumor(author, kind::MESSAGE, text, channel, epoch, unix_ms, vec![])
}

/// A reply: quotes the parent with a `q` tag citing the parent's *rumor* id
/// (never the outer wrap's, which differs per re-wrap).
pub fn build_reply(
    author: PublicKey,
    channel: &ChannelId,
    epoch: Epoch,
    text: &str,
    parent_rumor_id: &EventId,
    parent_author: &PublicKey,
    unix_ms: u64,
) -> UnsignedEvent {
    let q = Tag::custom(
        TagKind::q(),
        [parent_rumor_id.to_hex(), String::new(), parent_author.to_hex()],
    );
    build_rumor(author, kind::MESSAGE, text, channel, epoch, unix_ms, vec![q])
}

/// A kind 7 reaction (NIP-25 shape).
pub fn build_reaction(
    author: PublicKey,
    channel: &ChannelId,
    epoch: Epoch,
    reaction: &str,
    message_rumor_id: &EventId,
    message_author: &PublicKey,
    unix_ms: u64,
) -> UnsignedEvent {
    let tags = vec![
        Tag::custom(TagKind::e(), [message_rumor_id.to_hex()]),
        Tag::public_key(*message_author),
        Tag::custom(TagKind::k(), [kind::MESSAGE.to_string()]),
    ];
    build_rumor(author, kind::REACTION, reaction, channel, epoch, unix_ms, tags)
}

/// A kind 5 delete of one's own message (NIP-09 shape). Semantic within the
/// plane only — honored always, even post-Dissolution.
pub fn build_delete(
    author: PublicKey,
    channel: &ChannelId,
    epoch: Epoch,
    own_message_rumor_id: &EventId,
    unix_ms: u64,
) -> UnsignedEvent {
    let tags = vec![
        Tag::custom(TagKind::e(), [own_message_rumor_id.to_hex()]),
        Tag::custom(TagKind::k(), [kind::MESSAGE.to_string()]),
    ];
    build_rumor(author, kind::DELETE, "", channel, epoch, unix_ms, tags)
}

/// A kind 3302 edit of one's own message: `content` is the replacement text.
pub fn build_edit(
    author: PublicKey,
    channel: &ChannelId,
    epoch: Epoch,
    replacement: &str,
    own_message_rumor_id: &EventId,
    unix_ms: u64,
) -> UnsignedEvent {
    let tags = vec![Tag::custom(TagKind::e(), [own_message_rumor_id.to_hex()])];
    build_rumor(author, kind::EDIT, replacement, channel, epoch, unix_ms, tags)
}

/// A kind 23311 typing indicator: presence is the signal, the rumor carries
/// nothing, and every layer rides the ephemeral range.
pub fn build_typing(author: PublicKey, channel: &ChannelId, epoch: Epoch, unix_ms: u64) -> UnsignedEvent {
    build_rumor(author, kind::TYPING, "", channel, epoch, unix_ms, vec![])
}

/// The `ms` tag helper other planes share.
pub fn ms_tag(unix_ms: u64) -> Tag {
    let (_, remainder) = split_ms(unix_ms);
    Tag::custom(TagKind::Custom(TAG_MS.into()), [remainder.to_string()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::concord::v2::control::FoldMode;
    use crate::concord::v2::edition::parse_edition;
    use crate::concord::v2::stream::{self, SealForm};

    const NOW: u64 = 1_722_500_000_123;

    #[test]
    fn founding_genesis_folds_end_to_end() {
        let owner = Keys::generate();
        let Founded { community, genesis } =
            Community::found(owner.public_key(), "Vector", vec!["wss://a".into()], NOW).unwrap();

        // Wrap the genesis editions at the Control Plane address (plaintext
        // seals) and fold them back like any member would.
        let control = community.control_key();
        let mut fold = ControlFold::verified(
            community.id,
            community.owner,
            &community.owner_salt,
            FoldMode::Tracking,
        )
        .unwrap();
        for rumor in &genesis {
            let wrap = stream::wrap_rumor(&control, &owner, rumor, SealForm::Plaintext, Keys::generate().public_key()).unwrap();
            let opened = stream::open(&control, &wrap).unwrap();
            assert_eq!(opened.seal_form, SealForm::Plaintext, "Control Plane seals are plaintext");
            fold.ingest([parse_edition(&opened.rumor).unwrap()]);
        }

        assert_eq!(fold.metadata().unwrap().name, "Vector");
        let channels = fold.channels();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].1.name, "general");
        assert_eq!(channels[0].0, community.channels[0].id);
        assert!(!fold.is_public(), "genesis is Private: no live links");
    }

    #[test]
    fn join_from_bundle_and_message_roundtrip() {
        let owner = Keys::generate();
        let Founded { community, .. } =
            Community::found(owner.public_key(), "Vector", vec!["wss://a".into()], NOW).unwrap();
        let general = community.channels[0].id;

        // Founder mints a bundle; a member joins from it.
        let bundle = community.invite_bundle(&[general], None, None, None);
        let member_keys = Keys::generate();
        let joined = Community::join(bundle, NOW).unwrap();
        assert_eq!(joined.id, community.id);
        assert_eq!(joined.root_epoch, Epoch(0));

        // Both derive the same channel address (a Public Channel needs no
        // key delivery).
        let founder_gk = community.channel_key(&general).unwrap();
        let member_gk = joined.channel_key(&general).unwrap();
        assert_eq!(founder_gk.public_key(), member_gk.public_key());

        // Member sends; founder reads.
        let rumor = build_message(member_keys.public_key(), &general, Epoch(0), "Hey chat!", NOW);
        let wrap = stream::wrap_rumor(&member_gk, &member_keys, &rumor, SealForm::Encrypted, Keys::generate().public_key()).unwrap();
        let opened = stream::open(&founder_gk, &wrap).unwrap();
        stream::check_binding(&opened.rumor, &general, Epoch(0)).unwrap();
        assert_eq!(opened.rumor.content, "Hey chat!");
        assert_eq!(opened.author, member_keys.public_key());
        assert_eq!(opened.timestamp_ms(), Some(NOW));
    }

    #[test]
    fn expired_bundle_refuses_joining() {
        let owner = Keys::generate();
        let Founded { community, .. } = Community::found(owner.public_key(), "V", vec![], NOW).unwrap();
        let general = community.channels[0].id;
        let bundle = community.invite_bundle(&[general], Some(NOW - 1), None, None);
        assert!(matches!(Community::join(bundle, NOW), Err(InviteError::Expired)));
    }

    #[test]
    fn private_channel_grants_carry_keys_public_do_not() {
        let owner = Keys::generate();
        let Founded { mut community, .. } = Community::found(owner.public_key(), "V", vec![], NOW).unwrap();
        let general = community.channels[0].id;
        let testers = ChannelId([0x99; 32]);
        community.channels.push(Channel {
            id: testers,
            name: "testers".into(),
            private: true,
            deleted: false,
            key: Some(ChannelKey([0x42; 32])),
            epoch: Epoch(1),
        });

        let bundle = community.invite_bundle(&[general, testers], None, None, None);
        let public_grant = bundle.channels.iter().find(|c| c.name == "general").unwrap();
        let private_grant = bundle.channels.iter().find(|c| c.name == "testers").unwrap();
        assert!(public_grant.key.is_none());
        assert_eq!(private_grant.key.as_deref(), Some(crate::simd::hex::bytes_to_hex_32(&[0x42; 32]).as_str()));
        assert_eq!(private_grant.epoch, 1);

        let joined = Community::join(bundle, NOW).unwrap();
        assert_eq!(
            joined.channel_key(&testers).unwrap().public_key(),
            community.channel_key(&testers).unwrap().public_key()
        );
    }

    #[test]
    fn rekey_subscription_covers_private_channels_and_base() {
        let owner = Keys::generate();
        let Founded { mut community, .. } = Community::found(owner.public_key(), "V", vec![], NOW).unwrap();
        community.channels.push(Channel {
            id: ChannelId([0x99; 32]),
            name: "testers".into(),
            private: true,
            deleted: false,
            key: Some(ChannelKey([0x42; 32])),
            epoch: Epoch(1),
        });
        let authors = community.rekey_subscription_authors();
        // One private channel (next epoch 2) + the base (next epoch 1).
        assert_eq!(authors.len(), 2);
        assert_eq!(
            authors[0],
            derive::rekey_address(&community.root, &ChannelId([0x99; 32]), Epoch(2)).public_key()
        );
        assert_eq!(
            authors[1],
            derive::base_rekey_address(&community.root, &community.id, Epoch(1)).public_key()
        );
    }

    #[test]
    fn root_rotation_only_moves_forward() {
        let owner = Keys::generate();
        let Founded { mut community, .. } = Community::found(owner.public_key(), "V", vec![], NOW).unwrap();
        let old_root = community.root.clone();
        community.apply_new_root(CommunityRoot([0x50; 32]), Epoch(1));
        assert_eq!(community.root_epoch, Epoch(1));
        // A stale (lower-epoch) rotation never regresses the root.
        community.apply_new_root(CommunityRoot([0x60; 32]), Epoch(1));
        assert_eq!(community.root, CommunityRoot([0x50; 32]));
        assert_ne!(community.root, old_root);
    }

    #[test]
    fn chat_rumor_shapes_match_the_registry() {
        let author = Keys::generate().public_key();
        let parent_author = Keys::generate().public_key();
        let chan = ChannelId([0x77; 32]);
        let parent = EventId::from_slice(&[1; 32]).unwrap();

        let reply = build_reply(author, &chan, Epoch(0), "Welcome!", &parent, &parent_author, NOW);
        let q = reply.tags.iter().find(|t| t.kind() == TagKind::q()).unwrap();
        assert_eq!(q.as_slice()[1], parent.to_hex());
        assert_eq!(q.as_slice()[3], parent_author.to_hex());

        let reaction = build_reaction(author, &chan, Epoch(0), "🔥", &parent, &parent_author, NOW);
        assert_eq!(reaction.kind.as_u16(), kind::REACTION);
        assert_eq!(reaction.content, "🔥");
        let k = reaction.tags.iter().find(|t| t.kind() == TagKind::k()).and_then(|t| t.content());
        assert_eq!(k, Some("9"));

        let delete = build_delete(author, &chan, Epoch(0), &parent, NOW);
        assert_eq!(delete.kind.as_u16(), kind::DELETE);

        let edit = build_edit(author, &chan, Epoch(0), "fixed", &parent, NOW);
        assert_eq!(edit.kind.as_u16(), kind::EDIT);
        assert_eq!(edit.content, "fixed");

        let typing = build_typing(author, &chan, Epoch(0), NOW);
        assert_eq!(typing.kind.as_u16(), kind::TYPING);
        assert!(typing.content.is_empty());
        // All carry the binding.
        for rumor in [&reply, &reaction, &delete, &edit, &typing] {
            stream::check_binding(rumor, &chan, Epoch(0)).unwrap();
        }
    }

    #[test]
    fn apply_control_follows_the_fold_without_touching_keys() {
        let owner = Keys::generate();
        let Founded { mut community, genesis } = Community::found(owner.public_key(), "V", vec![], NOW).unwrap();
        let mut fold = ControlFold::new(community.id, community.owner, FoldMode::Tracking);
        fold.ingest(genesis.iter().map(|r| parse_edition(r).unwrap()));

        // Owner renames the community and the channel.
        let meta_head = fold.head(&community.id.0).unwrap();
        let rename = build_edition_rumor(
            owner.public_key(),
            vsk::COMMUNITY_METADATA,
            &community.id.0,
            2,
            Some(&meta_head.hash()),
            "{\"name\":\"Vector HQ\",\"relays\":[\"wss://new\"]}",
            NOW / 1000,
            None,
        );
        fold.ingest([parse_edition(&rename).unwrap()]);

        community.apply_control(&fold);
        assert_eq!(community.name, "Vector HQ");
        assert_eq!(community.relays, vec!["wss://new".to_string()]);
        assert_eq!(community.channels.len(), 1, "definitions follow the fold");
    }
}
