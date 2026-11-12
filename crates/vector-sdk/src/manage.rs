//! Running a Community: Roles, Categories, Channel settings and overwrites, member Roles,
//! Timeouts and Nicknames, pins, invites, relays, images and the Audit Log.
//!
//! Every action is checked the way every member's client will judge it, so a refusal comes back
//! as an error here instead of an edition everyone ignores.

use nostr_sdk::prelude::{PublicKey, ToBech32 as _};

use crate::{Channel, ChannelKind, Community, Member, Result, VectorError};
use vector_core::community::roles::OverwriteTarget;
use vector_core::VectorCore;

pub use vector_core::community::roles::{Permissions, Role as RoleInfo, RoleScope};
pub use vector_core::community::v2::changes::{CategoryInfo, ChangeKind, ChannelInfo, CommunityChange};
pub use vector_core::community::v2::service::{RelayCheck, RelayMove};
pub use vector_core::db::community::PublicInviteRecord as InviteLink;
pub use vector_core::{LinkPreview, NewChannel, NewCommunity, PostOptions};

fn hex32(id: &str, what: &str) -> Result<[u8; 32]> {
    vector_core::simd::hex::hex_to_bytes_32_checked(id).ok_or_else(|| VectorError::Other(format!("{what} must be 32-byte hex")))
}

fn npub_of_hex(h: &str) -> String {
    PublicKey::from_hex(h).ok().and_then(|p| p.to_bech32().ok()).unwrap_or_else(|| h.to_string())
}

fn hex_of(npub_or_hex: &str) -> Result<String> {
    PublicKey::parse(npub_or_hex).map(|p| p.to_hex()).map_err(|_| VectorError::Other(format!("{npub_or_hex} isn't an npub or hex pubkey")))
}

fn not_a_dm(c: &Channel) -> Result<()> {
    match c.kind {
        ChannelKind::Community => Ok(()),
        ChannelKind::Dm => Err(VectorError::Other("this is a Community channel setting; a DM has none".into())),
    }
}

// ============================================================================
// Roles
// ============================================================================

/// What a Role is made or edited to be. Fields left unset keep what the Role has.
///
/// ```
/// use vector_sdk::{Permissions, RoleSpec};
/// let mods = RoleSpec::new("Mods").color(0x59fcb3).permissions(Permissions::KICK | Permissions::MODERATE_MEMBERS).shown();
/// ```
#[derive(Debug, Clone, Default)]
pub struct RoleSpec {
    name: Option<String>,
    color: Option<u32>,
    permissions: Option<Permissions>,
    channel: Option<String>,
    shown: Option<bool>,
    mentionable: Option<bool>,
}

impl RoleSpec {
    pub fn new(name: impl Into<String>) -> Self {
        RoleSpec { name: Some(name.into()), ..Default::default() }
    }

    /// An edit that changes nothing until a setter says what.
    pub fn unchanged() -> Self {
        RoleSpec::default()
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// `0xRRGGBB`; `0` is the theme's default.
    pub fn color(mut self, rgb: u32) -> Self {
        self.color = Some(rgb);
        self
    }

    pub fn permissions(mut self, permissions: impl Into<Permissions>) -> Self {
        self.permissions = Some(permissions.into());
        self
    }

    /// A Role scoped to one Channel, on a Community still on the first roles model.
    pub fn in_channel(mut self, channel_id: impl Into<String>) -> Self {
        self.channel = Some(channel_id.into());
        self
    }

    /// Its holders list under its own heading in the member list.
    pub fn shown(mut self) -> Self {
        self.shown = Some(true);
        self
    }

    pub fn hidden(mut self) -> Self {
        self.shown = Some(false);
        self
    }

    /// Any member may @mention it.
    pub fn mentionable(mut self, yes: bool) -> Self {
        self.mentionable = Some(yes);
        self
    }
}

/// A handle to one Role of a Community. Obtained from [`Community::role`] or
/// [`Community::create_role`].
#[derive(Clone, Debug)]
pub struct Role {
    community_id: String,
    id: String,
}

impl Role {
    /// The role id (32-byte hex).
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn community_id(&self) -> &str {
        &self.community_id
    }

    /// What the Role is now, as this device has folded it.
    pub fn info(&self) -> Result<RoleInfo> {
        roster(&self.community_id)?.role(&self.id).cloned().ok_or_else(|| VectorError::NotFound(format!("role {}", self.id)))
    }

    /// Who holds it, as npubs.
    pub fn holders(&self) -> Result<Vec<String>> {
        let roster = roster(&self.community_id)?;
        let banned = vector_core::db::community::get_community_banlist(&self.community_id).unwrap_or_default();
        Ok(roster
            .grants
            .iter()
            .filter(|g| !banned.contains(&g.member) && g.role_ids.iter().any(|r| r.eq_ignore_ascii_case(&self.id)))
            .map(|g| npub_of_hex(&g.member))
            .collect())
    }

    /// Change what `spec` names; the rest stays. Needs Manage Roles and a rank above it.
    pub async fn edit(&self, spec: RoleSpec) -> Result<()> {
        let current = self.info()?;
        require(&self.community_id, Permissions::MANAGE_ROLES, "editing a role").await?;
        let channel = match (&spec.channel, &current.scope) {
            (Some(c), _) => Some(c.clone()),
            (None, RoleScope::Channel(c)) => Some(c.clone()),
            (None, RoleScope::Server) => None,
        };
        VectorCore
            .edit_role_shown(
                &self.community_id,
                &self.id,
                spec.name.as_deref().unwrap_or(&current.name),
                spec.color.unwrap_or(current.color),
                &spec.permissions.unwrap_or(current.permissions).0.to_string(),
                channel.as_deref(),
                spec.shown,
                spec.mentionable,
            )
            .await
    }

    pub async fn rename(&self, name: &str) -> Result<()> {
        self.edit(RoleSpec::unchanged().name(name)).await
    }

    pub async fn set_permissions(&self, permissions: impl Into<Permissions>) -> Result<()> {
        self.edit(RoleSpec::unchanged().permissions(permissions)).await
    }

    pub async fn set_color(&self, rgb: u32) -> Result<()> {
        self.edit(RoleSpec::unchanged().color(rgb)).await
    }

    /// Delete it: terminal, and it leaves every holder.
    pub async fn delete(&self) -> Result<()> {
        require(&self.community_id, Permissions::MANAGE_ROLES, "deleting a role").await?;
        VectorCore.delete_role(&self.community_id, &self.id).await
    }
}

fn roster(community_id: &str) -> Result<vector_core::community::roles::CommunityRoles> {
    vector_core::db::community::get_community_roles(community_id).map_err(VectorError::Db)
}

/// Refuse before publishing what every reader would ignore. The owner holds everything. A
/// refusal is checked again after catching the Community up, since a grant may not have
/// folded here yet.
async fn require(community_id: &str, needs: u64, action: &str) -> Result<()> {
    let me = VectorCore.my_npub().ok_or_else(|| VectorError::NotInitialized("not logged in".into()))?;
    if VectorCore.member_has_permission(community_id, &me, needs)? {
        return Ok(());
    }
    VectorCore::v2_inline_follow(&vector_core::community::CommunityId(hex32(community_id, "community_id")?)).await;
    if VectorCore.member_has_permission(community_id, &me, needs)? {
        return Ok(());
    }
    Err(VectorError::NotPermitted { needs: Permissions(needs), action: action.to_string() })
}

impl Community {
    pub(crate) fn held(&self) -> Result<vector_core::community::v2::community::CommunityV2> {
        vector_core::db::community::load_community_v2(&vector_core::community::CommunityId(hex32(&self.id, "community_id")?))
            .map_err(VectorError::Db)?
            .ok_or_else(|| VectorError::NotFound(format!("community {}", self.id)))
    }

    /// Every Role, top of the order first.
    pub fn roles(&self) -> Result<Vec<RoleInfo>> {
        let mut roles = roster(&self.id)?.roles;
        roles.sort_by(|a, b| a.position.cmp(&b.position).then_with(|| a.role_id.cmp(&b.role_id)));
        Ok(roles)
    }

    /// A handle to one Role by id.
    pub fn role(&self, role_id: impl Into<String>) -> Role {
        Role { community_id: self.id.clone(), id: role_id.into().to_ascii_lowercase() }
    }

    /// The first Role named `name`, ignoring case.
    pub fn role_named(&self, name: &str) -> Result<Option<Role>> {
        Ok(self.roles()?.into_iter().find(|r| r.name.eq_ignore_ascii_case(name)).map(|r| self.role(r.role_id)))
    }

    /// Make a Role at the bottom of the order. Needs Manage Roles, and grants only bits you hold.
    /// [`shown`](RoleSpec::shown) and [`mentionable`](RoleSpec::mentionable) apply as a second
    /// edit once it exists; should that fail, the Role stands without them.
    pub async fn create_role(&self, spec: RoleSpec) -> Result<Role> {
        require(&self.id, Permissions::MANAGE_ROLES, "creating a role").await?;
        let name = spec.name.clone().ok_or_else(|| VectorError::Other("a role needs a name".into()))?;
        let perms = spec.permissions.unwrap_or_default();
        let id = VectorCore.create_role(&self.id, &name, spec.color.unwrap_or(0), &perms.0.to_string(), spec.channel.as_deref()).await?;
        let role = self.role(id);
        // A second edit. The Role stands either way, and a retry would make another.
        if spec.shown.is_some() || spec.mentionable.is_some() {
            if let Err(e) = role.edit(RoleSpec { name: None, color: None, permissions: None, channel: spec.channel, ..spec }).await {
                vector_core::log_warn!("[sdk] role {} stands, but not shown or mentionable as asked: {e}", &role.id[..12.min(role.id.len())]);
            }
        }
        Ok(role)
    }

    /// Set the order of the Roles beneath you, top first. Returns how many moved.
    pub async fn reorder_roles(&self, top_first: &[&Role]) -> Result<usize> {
        require(&self.id, Permissions::MANAGE_ROLES, "reordering roles").await?;
        let ids: Vec<String> = top_first.iter().map(|r| r.id.clone()).collect();
        VectorCore.reorder_roles(&self.id, &ids).await
    }

    /// What every member holds before any Role. `None` before Roles v2.
    pub fn everyone_permissions(&self) -> Result<Option<Permissions>> {
        Ok(roster(&self.id)?.everyone)
    }

    pub async fn set_everyone_permissions(&self, permissions: impl Into<Permissions>) -> Result<()> {
        require(&self.id, Permissions::MANAGE_ROLES, "changing what everyone may do").await?;
        VectorCore.set_everyone_permissions(&self.id, &permissions.into().0.to_string()).await
    }

    /// The Channel it opened with, which anchors its place in a chat list.
    pub fn primary_channel(&self) -> Option<Channel> {
        let held = self.held().ok()?;
        let id = vector_core::simd::hex::bytes_to_hex_32(&held.primary_channel()?.id.0);
        Some(self.channel(id))
    }

    // ---- Categories ----

    /// Every Category, in sidebar order.
    pub fn categories(&self) -> Result<Vec<CategoryInfo>> {
        Ok(vector_core::community::v2::changes::category_infos(&self.held()?))
    }

    pub fn category(&self, category_id: impl Into<String>) -> Category {
        Category { community_id: self.id.clone(), id: category_id.into().to_ascii_lowercase() }
    }

    /// Make a Category at the bottom of the sidebar. Needs Manage Channels.
    pub async fn create_category(&self, name: &str) -> Result<Category> {
        require(&self.id, Permissions::MANAGE_CHANNELS, "creating a category").await?;
        let id = VectorCore.create_category(&self.id, name).await?;
        Ok(self.category(id))
    }

    // ---- Channels ----

    /// Every Channel with its settings, in sidebar order, readable or not.
    pub fn channel_infos(&self) -> Result<Vec<ChannelInfo>> {
        Ok(vector_core::community::v2::changes::channel_infos(&self.held()?))
    }

    /// Make a Channel as [`NewChannel`] shapes it: private, in a Category, a `voice` or `forum`
    /// view, a topic. Needs Manage Channels. The view and topic are a second edit: should it
    /// fail, the Channel stands without them (check [`Channel::info`]), so a retry doesn't make two.
    pub async fn create_channel_with(&self, name: &str, opts: NewChannel) -> Result<Channel> {
        require(&self.id, Permissions::MANAGE_CHANNELS, "creating a channel").await?;
        let id = VectorCore.create_channel_with(&self.id, name, &opts).await?;
        Ok(self.channel(id))
    }

    // ---- Members ----

    /// The Timeouts running here: `(npub, until unix seconds)`.
    pub fn timeouts(&self) -> Result<Vec<(String, u64)>> {
        let held = self.held()?;
        Ok(vector_core::community::v2::service::community_timeouts(&held)
            .map_err(VectorError::Other)?
            .into_iter()
            .filter_map(|(pk, (_, until))| Some((pk.to_bech32().ok()?, until)))
            .collect())
    }

    /// Members with a Nickname here: `(npub, nickname)`.
    pub fn nicknames(&self) -> Result<Vec<(String, String)>> {
        let held = self.held()?;
        Ok(vector_core::community::v2::service::community_nicknames(&held)
            .map_err(VectorError::Other)?
            .into_iter()
            .filter_map(|(pk, nick)| Some((pk.to_bech32().ok()?, nick)))
            .collect())
    }

    // ---- Invites ----

    /// The invite links you've minted here that still stand.
    pub fn invite_links(&self) -> Result<Vec<InviteLink>> {
        VectorCore.list_public_invites(&self.id)
    }

    /// Revoke one invite link by its token (from [`invite_links`](Self::invite_links)).
    pub async fn revoke_invite(&self, token: &str) -> Result<()> {
        VectorCore.revoke_public_invite(&self.id, token).await
    }

    /// Revoke every invite link anyone minted here. Returns `(revoked, failed)`.
    pub async fn revoke_all_invites(&self) -> Result<(usize, usize)> {
        VectorCore.revoke_all_public_invites(&self.id).await
    }

    // ---- Settings ----

    /// The relays it lives on.
    pub fn relays(&self) -> Result<Vec<String>> {
        Ok(self.held()?.relays)
    }

    /// Ask each of `relays` whether it would hold this Community, before moving it there.
    pub async fn check_relays(&self, relays: &[String]) -> Result<Vec<RelayCheck>> {
        VectorCore.check_community_relays(&self.id, relays).await
    }

    /// Move it onto `relays`, copying what the old ones hold. Needs Manage Community.
    pub async fn move_relays(&self, relays: &[String]) -> Result<RelayMove> {
        require(&self.id, Permissions::MANAGE_METADATA, "moving the community's relays").await?;
        VectorCore.move_community_relays(&self.id, relays).await
    }

    /// The iroh relays its voice Channels use; empty means iroh's defaults.
    pub fn voice_relays(&self) -> Result<Vec<String>> {
        Ok(self.held()?.voice_relays())
    }

    /// Set the voice relays (https, at most eight; empty restores the defaults). Returns them
    /// as stored.
    pub async fn set_voice_relays(&self, relays: &[String]) -> Result<Vec<String>> {
        require(&self.id, Permissions::MANAGE_METADATA, "setting the voice relays").await?;
        VectorCore.set_voice_relays(&self.id, relays).await
    }

    /// The icon's image bytes, downloaded and checked; `None` when it has none.
    pub async fn icon(&self) -> Result<Option<Vec<u8>>> {
        VectorCore.community_image(&self.id, false).await
    }

    /// The banner's image bytes; `None` when it has none.
    pub async fn banner(&self) -> Result<Option<Vec<u8>>> {
        VectorCore.community_image(&self.id, true).await
    }

    /// Set the icon from PNG, JPEG, GIF or WebP bytes. They are stripped of metadata and fitted
    /// to 512px first, and only members can open the result. Needs Manage Community.
    pub async fn set_icon(&self, image: &[u8]) -> Result<()> {
        require(&self.id, Permissions::MANAGE_METADATA, "setting the icon").await?;
        VectorCore.set_community_image(&self.id, image, false).await.map(|_| ())
    }

    /// Set the banner, fitted to 1500px. Needs Manage Community.
    pub async fn set_banner(&self, image: &[u8]) -> Result<()> {
        require(&self.id, Permissions::MANAGE_METADATA, "setting the banner").await?;
        VectorCore.set_community_image(&self.id, image, true).await.map(|_| ())
    }

    pub async fn remove_icon(&self) -> Result<()> {
        require(&self.id, Permissions::MANAGE_METADATA, "removing the icon").await?;
        VectorCore.remove_community_image(&self.id, false).await
    }

    pub async fn remove_banner(&self) -> Result<()> {
        require(&self.id, Permissions::MANAGE_METADATA, "removing the banner").await?;
        VectorCore.remove_community_image(&self.id, true).await
    }

    // ---- Recovery ----

    /// Roll the community keys forward, keeping only `retain` (npubs) and the staff. An empty
    /// list rotates without removing anyone: right when an invite link leaked but every member
    /// is real.
    pub async fn refound(&self, retain: &[&str]) -> Result<()> {
        VectorCore.refound_community(&self.id, retain).await
    }

    /// Send the current keys again to members (npubs) stranded on an old epoch.
    pub async fn rescue_stranded(&self, npubs: &[&str]) -> Result<serde_json::Value> {
        VectorCore.rescue_stranded_members(&self.id, npubs).await
    }

    /// Rebuild the roster from what the Control Plane holds, healing a roster folded wrong.
    pub async fn repair_roster(&self) -> Result<serde_json::Value> {
        VectorCore.repair_community_roster(&self.id).await
    }

    /// Why this device is on the epoch it's on: a diagnostic for a member who can't read.
    pub async fn explain_epoch(&self) -> Result<serde_json::Value> {
        VectorCore.explain_epoch_state(&self.id).await
    }

    /// Who joined in a burst and what they posted: the evidence a raid response acts on.
    pub fn raid_report(&self) -> Result<serde_json::Value> {
        VectorCore.check_community_raid(&self.id)
    }
}

// ============================================================================
// Categories
// ============================================================================

/// A handle to one Category. Obtained from [`Community::category`] or
/// [`Community::create_category`].
#[derive(Clone, Debug)]
pub struct Category {
    community_id: String,
    id: String,
}

impl Category {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn info(&self) -> Result<CategoryInfo> {
        Community { core: VectorCore, id: self.community_id.clone() }
            .categories()?
            .into_iter()
            .find(|k| k.category_id == self.id)
            .ok_or_else(|| VectorError::NotFound(format!("category {}", self.id)))
    }

    /// Its Channels, in sidebar order.
    pub fn channels(&self) -> Result<Vec<ChannelInfo>> {
        Ok(Community { core: VectorCore, id: self.community_id.clone() }
            .channel_infos()?
            .into_iter()
            .filter(|c| c.category_id.as_deref() == Some(self.id.as_str()))
            .collect())
    }

    pub async fn rename(&self, name: &str) -> Result<()> {
        require(&self.community_id, Permissions::MANAGE_CHANNELS, "renaming a category").await?;
        VectorCore.rename_category(&self.community_id, &self.id, name).await
    }

    /// Move it to `index` in the sidebar's Categories.
    pub async fn move_to(&self, index: usize) -> Result<()> {
        require(&self.community_id, Permissions::MANAGE_CHANNELS, "moving a category").await?;
        VectorCore.move_category(&self.community_id, &self.id, index).await
    }

    /// Delete it: terminal. Its Channels stay, uncategorized.
    pub async fn delete(&self) -> Result<()> {
        require(&self.community_id, Permissions::MANAGE_CHANNELS, "deleting a category").await?;
        VectorCore.delete_category(&self.community_id, &self.id).await
    }

    /// Set one overwrite on the Category, which every Channel synced to it takes.
    pub async fn set_overwrite(&self, overwrite: Overwrite) -> Result<()> {
        overwrite.apply(&self.community_id, &self.id).await
    }
}

// ============================================================================
// Overwrites
// ============================================================================

/// Bits allowed and denied in one Channel or Category, for everyone, one Role or one member,
/// on top of what they hold server-wide. No bits removes the overwrite.
///
/// ```
/// use vector_sdk::{Overwrite, Permissions};
/// let read_only = Overwrite::everyone().deny(Permissions::SEND_MESSAGES);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overwrite {
    pub target: OverwriteTarget,
    pub allow: Permissions,
    pub deny: Permissions,
}

impl Overwrite {
    pub fn everyone() -> Self {
        Overwrite { target: OverwriteTarget::Everyone, allow: Permissions(0), deny: Permissions(0) }
    }

    pub fn role(role: &Role) -> Self {
        Overwrite { target: OverwriteTarget::Role(role.id.clone()), ..Overwrite::everyone() }
    }

    /// For one member, by npub or hex.
    pub fn member(npub: impl Into<String>) -> Self {
        Overwrite { target: OverwriteTarget::Member(npub.into()), ..Overwrite::everyone() }
    }

    pub fn allow(mut self, bits: impl Into<Permissions>) -> Self {
        self.allow = self.allow | bits.into();
        self
    }

    pub fn deny(mut self, bits: impl Into<Permissions>) -> Self {
        self.deny = self.deny | bits.into();
        self
    }

    async fn apply(&self, community_id: &str, scope_id: &str) -> Result<()> {
        require(community_id, Permissions::MANAGE_ROLES, "setting a permission overwrite").await?;
        let (kind, id) = match &self.target {
            OverwriteTarget::Everyone => ("everyone", None),
            OverwriteTarget::Role(id) => ("role", Some(id.as_str())),
            OverwriteTarget::Member(m) => ("member", Some(m.as_str())),
        };
        VectorCore
            .set_channel_overwrite(community_id, scope_id, kind, id, &self.allow.0.to_string(), &self.deny.0.to_string())
            .await
    }
}

impl Channel {
    fn community_id(&self) -> Result<String> {
        not_a_dm(self)?;
        vector_core::db::community::community_id_for_channel(&self.id)
            .map_err(VectorError::Db)?
            .ok_or_else(|| VectorError::NotFound(format!("community channel {}", self.id)))
    }

    /// Its settings: name, privacy, Category, topic and view.
    pub fn info(&self) -> Result<ChannelInfo> {
        let cid = self.community_id()?;
        Community { core: VectorCore, id: cid }
            .channel_infos()?
            .into_iter()
            .find(|c| c.channel_id == self.id)
            .ok_or_else(|| VectorError::NotFound(format!("channel {}", self.id)))
    }

    async fn edit_details(&self, name: Option<&str>, topic: Option<Option<&str>>, view: Option<&str>) -> Result<()> {
        let info = self.info()?;
        let cid = self.community_id()?;
        require(&cid, Permissions::MANAGE_CHANNELS, "editing a channel").await?;
        let topic = topic.unwrap_or(info.topic.as_deref());
        VectorCore.edit_channel_details(&cid, &self.id, name.unwrap_or(&info.name), topic, view.unwrap_or(&info.view)).await
    }

    pub async fn rename(&self, name: &str) -> Result<()> {
        self.edit_details(Some(name), None, None).await
    }

    /// Set the topic shown under its name; `None` clears it.
    pub async fn set_topic(&self, topic: Option<&str>) -> Result<()> {
        self.edit_details(None, Some(topic), None).await
    }

    /// How clients open it: `chat`, `forum` or `voice`. Any other name is an error.
    pub async fn set_view(&self, view: &str) -> Result<()> {
        self.edit_details(None, None, Some(view)).await
    }

    /// Make it read-only for everyone (an announcement channel), or open it again. Roles whose
    /// overwrite here allows Send Messages still post. Needs Manage Roles.
    ///
    /// ```no_run
    /// # async fn f(news: vector_sdk::Channel, mods: vector_sdk::Role) -> vector_sdk::Result<()> {
    /// use vector_sdk::{Overwrite, Permissions};
    /// news.set_read_only(true).await?;
    /// news.set_overwrite(Overwrite::role(&mods).allow(Permissions::SEND_MESSAGES)).await?;
    /// # Ok(()) }
    /// ```
    pub async fn set_read_only(&self, read_only: bool) -> Result<()> {
        let send = Permissions(Permissions::SEND_MESSAGES);
        let mut everyone = self.overwrites()?.into_iter().find(|o| o.target == OverwriteTarget::Everyone).unwrap_or_else(Overwrite::everyone);
        if read_only {
            everyone.deny = everyone.deny | send;
            everyone.allow = Permissions(everyone.allow.0 & !send.0);
        } else {
            everyone.deny = Permissions(everyone.deny.0 & !send.0);
        }
        self.set_overwrite(everyone).await
    }

    /// File it under `category` (or none) at `index` among that Category's Channels.
    pub async fn move_to(&self, category: Option<&Category>, index: usize) -> Result<()> {
        let cid = self.community_id()?;
        require(&cid, Permissions::MANAGE_CHANNELS, "moving a channel").await?;
        VectorCore.move_channel(&cid, &self.id, category.map(|k| k.id.as_str()), index).await
    }

    /// Its own overwrites (a synced Channel resolves against its Category's instead).
    pub fn overwrites(&self) -> Result<Vec<Overwrite>> {
        let cid = self.community_id()?;
        let chan = self.id.to_ascii_lowercase();
        Ok(roster(&cid)?
            .overwrites
            .into_iter()
            .filter(|o| o.scope == chan)
            .map(|o| Overwrite {
                target: match o.target {
                    OverwriteTarget::Member(h) => OverwriteTarget::Member(npub_of_hex(&h)),
                    t => t,
                },
                allow: o.allow,
                deny: o.deny,
            })
            .collect())
    }

    /// Set one overwrite. Needs Manage Roles.
    pub async fn set_overwrite(&self, overwrite: Overwrite) -> Result<()> {
        let cid = self.community_id()?;
        overwrite.apply(&cid, &self.id).await
    }

    /// Remove the overwrite for `target`.
    pub async fn clear_overwrite(&self, target: OverwriteTarget) -> Result<()> {
        self.set_overwrite(Overwrite { target, allow: Permissions(0), deny: Permissions(0) }).await
    }

    /// Take `category`'s overwrites instead of its own, or (`None`) its own again.
    pub async fn sync_permissions(&self, category: Option<&Category>) -> Result<()> {
        let cid = self.community_id()?;
        require(&cid, Permissions::MANAGE_ROLES, "syncing a channel's permissions").await?;
        VectorCore.set_channel_sync(&cid, &self.id, category.map(|k| k.id.as_str())).await
    }

    /// What `npub` may do here: their Roles, then this Channel's overwrites.
    pub fn permissions_of(&self, npub: &str) -> Result<Permissions> {
        let cid = self.community_id()?;
        let held = Community { core: VectorCore, id: cid.clone() }.held()?;
        let owner = held.owner().ok().map(|o| o.to_hex());
        let private = held.channel(&vector_core::community::ChannelId(hex32(&self.id, "channel_id")?)).is_some_and(|c| c.private);
        Ok(roster(&cid)?.channel_permissions(owner.as_deref(), &hex_of(npub)?, &self.id, private))
    }

    // ---- pins ----

    /// The pinned messages, in the curator's order. `Err` when the list exists but this device
    /// can't open it yet: never read that as "no pins".
    pub fn pins(&self) -> Result<Vec<Pin>> {
        let cid = self.community_id()?;
        let held = Community { core: VectorCore, id: cid }.held()?;
        let pins = vector_core::community::v2::service::read_channel_pins(&held, &vector_core::community::ChannelId(hex32(&self.id, "channel_id")?))
            .map_err(VectorError::Other)?;
        if pins.sealed {
            return Err(VectorError::Other("the pins are sealed under a key this device doesn't hold yet".into()));
        }
        Ok(pins
            .pins
            .into_iter()
            .map(|p| Pin { message_id: p.rumor_id, author: npub_of_hex(&p.author), content: p.content, at_ms: p.ms })
            .collect())
    }

    /// Pin a message. Needs Pin Messages.
    pub async fn pin(&self, message_id: &str) -> Result<()> {
        let cid = self.community_id()?;
        require(&cid, Permissions::PIN_MESSAGES, "pinning a message").await?;
        VectorCore.pin_community_message(&cid, &self.id, message_id).await
    }

    pub async fn unpin(&self, message_id: &str) -> Result<()> {
        let cid = self.community_id()?;
        require(&cid, Permissions::PIN_MESSAGES, "unpinning a message").await?;
        VectorCore.unpin_community_message(&cid, &self.id, message_id).await
    }
}

/// A pinned message.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Pin {
    pub message_id: String,
    /// npub.
    pub author: String,
    pub content: String,
    pub at_ms: u64,
}

// ============================================================================
// Members
// ============================================================================

impl Member {
    fn hex(&self) -> Result<String> {
        hex_of(&self.npub)
    }

    /// Their Roles, top of the order first.
    pub fn roles(&self) -> Result<Vec<RoleInfo>> {
        let roster = roster(&self.community_id)?;
        let me = self.hex()?;
        let mut roles: Vec<RoleInfo> = roster.roles_of(&me).cloned().collect();
        roles.sort_by(|a, b| a.position.cmp(&b.position).then_with(|| a.role_id.cmp(&b.role_id)));
        Ok(roles)
    }

    /// Whether they hold `role`.
    pub fn has_role(&self, role: &Role) -> Result<bool> {
        Ok(self.roles()?.iter().any(|r| r.role_id.eq_ignore_ascii_case(&role.id)))
    }

    /// Give them exactly these Roles. Needs Manage Roles and a rank above each Role touched.
    pub async fn set_roles(&self, roles: &[&Role]) -> Result<()> {
        require(&self.community_id, Permissions::MANAGE_ROLES, "changing a member's roles").await?;
        let ids: Vec<String> = roles.iter().map(|r| r.id.clone()).collect();
        VectorCore.set_member_roles(&self.community_id, &self.npub, ids).await
    }

    /// Give them `role`, keeping every other Role they hold by the freshest view.
    pub async fn add_role(&self, role: &Role) -> Result<()> {
        require(&self.community_id, Permissions::MANAGE_ROLES, "giving a member a role").await?;
        VectorCore.modify_member_roles(&self.community_id, &self.npub, std::slice::from_ref(&role.id), &[]).await
    }

    /// Take `role` from them, keeping every other Role they hold by the freshest view.
    pub async fn remove_role(&self, role: &Role) -> Result<()> {
        require(&self.community_id, Permissions::MANAGE_ROLES, "taking a member's role").await?;
        VectorCore.modify_member_roles(&self.community_id, &self.npub, &[], std::slice::from_ref(&role.id)).await
    }

    /// What they may do community-wide.
    pub fn permissions(&self) -> Result<Permissions> {
        let roster = roster(&self.community_id)?;
        let owner = Community { core: VectorCore, id: self.community_id.clone() }.held()?.owner().ok().map(|o| o.to_hex());
        let me = self.hex()?;
        if owner.as_deref() == Some(me.as_str()) {
            return Ok(Permissions(u64::MAX));
        }
        Ok(roster.roles_of(&me).fold(roster.everyone.unwrap_or_default(), |acc, r| acc.union(r.permissions)))
    }

    /// Silence them for `duration` (at most 28 days): they can read, not post, react or speak.
    /// Needs Moderate Members and a rank above them.
    pub async fn timeout(&self, duration: std::time::Duration) -> Result<()> {
        require(&self.community_id, Permissions::MODERATE_MEMBERS, "timing out a member").await?;
        VectorCore.timeout_member(&self.community_id, &self.npub, duration.as_secs().max(1)).await
    }

    /// Lift a running Timeout.
    pub async fn remove_timeout(&self) -> Result<()> {
        require(&self.community_id, Permissions::MODERATE_MEMBERS, "lifting a timeout").await?;
        VectorCore.timeout_member(&self.community_id, &self.npub, 0).await
    }

    /// When their Timeout ends (unix seconds), if one is running.
    pub fn timed_out_until(&self) -> Result<Option<u64>> {
        let community = Community { core: VectorCore, id: self.community_id.clone() };
        let me = self.hex()?;
        Ok(community.timeouts()?.into_iter().find(|(n, _)| hex_of(n).is_ok_and(|h| h == me)).map(|(_, until)| until))
    }

    /// What they're called in this Community, when they have a Nickname here.
    pub fn nickname(&self) -> Option<String> {
        VectorCore.member_nickname(&self.community_id, &self.npub)
    }

    /// Set their Nickname here (`""` clears it): your own, or one beneath you with Manage
    /// Nicknames. Everyone in the Community sees it, unlike [`VectorBot::set_nickname`](crate::VectorBot::set_nickname).
    pub async fn set_nickname(&self, nickname: &str) -> Result<()> {
        VectorCore.set_community_nickname(&self.community_id, &self.npub, nickname).await
    }
}

// ============================================================================
// The Audit Log
// ============================================================================

/// What an Audit Log entry records.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuditAction {
    CommunityCreate,
    CommunityUpdate,
    RolesActivate,
    EveryoneUpdate,
    RoleCreate,
    RoleUpdate,
    RoleDelete,
    MemberRolesUpdate,
    MemberBan,
    MemberUnban,
    BanlistUpdate,
    MemberKick,
    MemberTimeout,
    MemberTimeoutRemove,
    MemberNickUpdate,
    ChannelCreate,
    ChannelUpdate,
    ChannelDelete,
    ChannelSync,
    ChannelUnsync,
    CategoryCreate,
    CategoryUpdate,
    CategoryDelete,
    OverwriteCreate,
    OverwriteUpdate,
    OverwriteDelete,
    MessagePin,
    MessageUnpin,
    PinsUpdate,
    MessageDelete,
    InviteCreate,
    InviteDelete,
    KeysRotate,
    VoiceMute,
    VoiceUnmute,
    VoiceDeafen,
    VoiceUndeafen,
    VoiceMove,
    VoiceDisconnect,
    /// An action this SDK version doesn't name yet.
    Other(String),
}

impl AuditAction {
    fn parse(s: &str) -> Self {
        use AuditAction::*;
        match s {
            "community_create" => CommunityCreate,
            "community_update" => CommunityUpdate,
            "roles_activate" => RolesActivate,
            "everyone_update" => EveryoneUpdate,
            "role_create" => RoleCreate,
            "role_update" => RoleUpdate,
            "role_delete" => RoleDelete,
            "member_roles_update" => MemberRolesUpdate,
            "member_ban" => MemberBan,
            "member_unban" => MemberUnban,
            "banlist_update" => BanlistUpdate,
            "member_kick" => MemberKick,
            "member_timeout" => MemberTimeout,
            "member_timeout_remove" => MemberTimeoutRemove,
            "member_nick_update" => MemberNickUpdate,
            "channel_create" => ChannelCreate,
            "channel_update" => ChannelUpdate,
            "channel_delete" => ChannelDelete,
            "channel_sync" => ChannelSync,
            "channel_unsync" => ChannelUnsync,
            "category_create" => CategoryCreate,
            "category_update" => CategoryUpdate,
            "category_delete" => CategoryDelete,
            "overwrite_create" => OverwriteCreate,
            "overwrite_update" => OverwriteUpdate,
            "overwrite_delete" => OverwriteDelete,
            "message_pin" => MessagePin,
            "message_unpin" => MessageUnpin,
            "pins_update" => PinsUpdate,
            "message_delete" => MessageDelete,
            "invite_create" => InviteCreate,
            "invite_delete" => InviteDelete,
            "keys_rotate" => KeysRotate,
            "voice_mute" => VoiceMute,
            "voice_unmute" => VoiceUnmute,
            "voice_deafen" => VoiceDeafen,
            "voice_undeafen" => VoiceUndeafen,
            "voice_move" => VoiceMove,
            "voice_disconnect" => VoiceDisconnect,
            other => Other(other.to_string()),
        }
    }
}

/// What an act was done to.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuditTarget {
    Member { npub: String },
    Everyone,
    Role { id: String, name: Option<String> },
    Channel { id: String, name: Option<String> },
    Category { id: String, name: Option<String> },
    /// A message, with its author and Channel when known.
    Message { id: Option<String>, author: Option<String>, channel_id: Option<String> },
    /// An overwrite: who it's for, and the Channel or Category it sits in.
    Overwrite { target: Box<AuditTarget>, scope: Box<AuditTarget> },
    /// A shape this SDK version doesn't name yet.
    Other(serde_json::Value),
}

impl AuditTarget {
    fn parse(v: &serde_json::Value) -> Self {
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
        match v.get("kind").and_then(|k| k.as_str()) {
            Some("member") => AuditTarget::Member { npub: s("npub").unwrap_or_default() },
            Some("everyone") => AuditTarget::Everyone,
            Some("role") => AuditTarget::Role { id: s("id").unwrap_or_default(), name: s("name") },
            Some("channel") => AuditTarget::Channel { id: s("id").unwrap_or_default(), name: s("name") },
            Some("category") => AuditTarget::Category { id: s("id").unwrap_or_default(), name: s("name") },
            Some("message") => AuditTarget::Message {
                id: s("id"),
                author: s("author"),
                channel_id: v.get("channel").and_then(|c| c.get("id")).and_then(|i| i.as_str()).map(str::to_string),
            },
            Some("overwrite") => AuditTarget::Overwrite {
                target: Box::new(v.get("for").map(AuditTarget::parse).unwrap_or(AuditTarget::Other(serde_json::Value::Null))),
                scope: Box::new(v.get("in").map(AuditTarget::parse).unwrap_or(AuditTarget::Other(serde_json::Value::Null))),
            },
            _ => AuditTarget::Other(v.clone()),
        }
    }
}

/// One act in a Community's Audit Log.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AuditEntry {
    pub id: String,
    /// When its author dated it, unix milliseconds.
    pub at_ms: u64,
    /// The npub that did it, when known.
    pub actor: Option<String>,
    pub action: AuditAction,
    pub target: Option<AuditTarget>,
    /// What changed, one object per field: `{ field, old, new }` for text, `{ field, added,
    /// removed }` for permission bits and Roles.
    pub changes: Vec<serde_json::Value>,
    /// What stood before was compacted away, so what changed can't be told.
    pub partial: bool,
}

/// A Community's Audit Log, newest first.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AuditLog {
    pub entries: Vec<AuditEntry>,
    /// History from before a key rotation this device didn't live through is missing.
    pub compacted: bool,
}

impl Community {
    /// Who did what and when: every Control change this device accepted, the Guestbook's
    /// moderation, and the moderator deletes and voice acts it saw. Needs View Audit Log to be
    /// meaningful to others; reading it is local plus one Control Plane read.
    pub async fn audit_log(&self) -> Result<AuditLog> {
        let log = VectorCore.community_audit_log(&self.id).await?;
        Ok(AuditLog {
            compacted: log.compacted,
            entries: log
                .entries
                .into_iter()
                .map(|e| AuditEntry {
                    id: e.id,
                    at_ms: e.at,
                    actor: e.actor,
                    action: AuditAction::parse(e.action),
                    target: e.target.as_ref().map(AuditTarget::parse),
                    changes: e.changes,
                    partial: e.partial,
                })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_core_writes_has_a_name() {
        for a in [
            "community_create", "community_update", "roles_activate", "everyone_update", "role_create", "role_update", "role_delete",
            "member_roles_update", "member_ban", "member_unban", "banlist_update", "member_kick", "member_timeout",
            "member_timeout_remove", "member_nick_update", "channel_create", "channel_update", "channel_delete", "channel_sync",
            "channel_unsync", "category_create", "category_update", "category_delete", "overwrite_create", "overwrite_update",
            "overwrite_delete", "message_pin", "message_unpin", "pins_update", "message_delete", "invite_create", "invite_delete",
            "keys_rotate", "voice_mute", "voice_unmute", "voice_deafen", "voice_undeafen", "voice_move", "voice_disconnect",
        ] {
            assert!(!matches!(AuditAction::parse(a), AuditAction::Other(_)), "{a} reads as unknown");
        }
        assert_eq!(AuditAction::parse("future_thing"), AuditAction::Other("future_thing".into()));
    }

    #[test]
    fn an_overwrite_target_reads_both_halves() {
        let v = serde_json::json!({ "kind": "overwrite", "for": { "kind": "role", "id": "ab", "name": "Mods" }, "in": { "kind": "channel", "id": "cd", "name": "general" } });
        let AuditTarget::Overwrite { target, scope } = AuditTarget::parse(&v) else { panic!() };
        assert_eq!(*target, AuditTarget::Role { id: "ab".into(), name: Some("Mods".into()) });
        assert_eq!(*scope, AuditTarget::Channel { id: "cd".into(), name: Some("general".into()) });
    }

    #[test]
    fn overwrite_builders_accumulate_bits() {
        let o = Overwrite::everyone().deny(Permissions::SEND_MESSAGES).deny(Permissions::ADD_REACTIONS).allow(Permissions::VIEW_CHANNEL);
        assert_eq!(o.deny.0, Permissions::SEND_MESSAGES | Permissions::ADD_REACTIONS);
        assert_eq!(o.allow.0, Permissions::VIEW_CHANNEL);
    }

    #[tokio::test]
    async fn a_dm_has_no_channel_settings() {
        let dm = Channel { core: VectorCore, id: "npub1example".into(), kind: ChannelKind::Dm };
        assert!(dm.info().is_err());
        assert!(dm.pin(&"aa".repeat(32)).await.is_err());
    }
}
