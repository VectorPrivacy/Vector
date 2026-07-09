use std::sync::Arc;
use tokio::sync::Mutex;
use rmcp::{
    ServerHandler,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::*,
    schemars, tool, tool_handler, tool_router,
    ErrorData as McpError,
};
use vector_core::VectorCore;

use crate::handler::BufferedMessage;

// ============================================================================
// Request types — each becomes JSON Schema via schemars
// ============================================================================

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SendDmRequest {
    #[schemars(description = "Recipient's npub (e.g. npub1abc...)")]
    pub to_npub: String,
    #[schemars(description = "Message content")]
    pub content: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SendFileRequest {
    #[schemars(description = "Recipient's npub")]
    pub to_npub: String,
    #[schemars(description = "Absolute path to the file to send")]
    pub file_path: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetMessagesRequest {
    #[schemars(description = "Chat ID (npub for DMs, group_id for groups)")]
    pub chat_id: String,
    #[schemars(description = "Maximum number of messages to return")]
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[schemars(description = "Offset from most recent")]
    #[serde(default)]
    pub offset: usize,
}

fn default_limit() -> usize { 50 }

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NpubRequest {
    #[schemars(description = "User's npub")]
    pub npub: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddAccountRequest {
    #[schemars(description = "Optional nsec to import. OMIT to generate a fresh, random identity (preferred for test accounts — keeps secret keys out of the conversation).")]
    #[serde(default)]
    pub nsec: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetNicknameRequest {
    #[schemars(description = "User's npub")]
    pub npub: String,
    #[schemars(description = "Nickname to set")]
    pub nickname: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SyncDmsRequest {
    #[schemars(description = "Number of days to sync (e.g. 7 for last week). Omit for full history sync.")]
    pub since_days: Option<u64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdateProfileRequest {
    #[schemars(description = "Display name")]
    pub name: String,
    #[schemars(description = "Avatar URL")]
    #[serde(default)]
    pub avatar: String,
    #[schemars(description = "Banner URL")]
    #[serde(default)]
    pub banner: String,
    #[schemars(description = "About/bio text")]
    #[serde(default)]
    pub about: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct JoinCommunityRequest {
    #[schemars(description = "Public invite URL (e.g. https://vectorapp.io/invite#...)")]
    pub invite_url: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CommunityIdRequest {
    #[schemars(description = "Community id (hex)")]
    pub community_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RevokePublicInviteRequest {
    #[schemars(description = "Community id (hex)")]
    pub community_id: String,
    #[schemars(description = "Hex token of the invite link to revoke (from list_public_invites)")]
    pub token: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateCommunityRequest {
    #[schemars(description = "Name for the new community")]
    pub name: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CommunityMemberRequest {
    #[schemars(description = "Community id (hex)")]
    pub community_id: String,
    #[schemars(description = "The target member's npub")]
    pub npub: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EditCommunityMetadataRequest {
    #[schemars(description = "Community id (hex)")]
    pub community_id: String,
    #[schemars(description = "New name (omit to leave unchanged)")]
    #[serde(default)]
    pub name: Option<String>,
    #[schemars(description = "New description (empty string clears it; omit to leave unchanged)")]
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SendCommunityMessageRequest {
    #[schemars(description = "Channel id (the hex chat id of a Community channel)")]
    pub channel_id: String,
    #[schemars(description = "Message content")]
    pub content: String,
    #[schemars(description = "Optional inner id of a message to reply to")]
    #[serde(default)]
    pub replied_to: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SyncCommunityChannelRequest {
    #[schemars(description = "Channel id to sync the latest page of")]
    pub channel_id: String,
    #[schemars(description = "Max messages to fetch (default 20)")]
    #[serde(default = "default_page")]
    pub limit: usize,
}

fn default_page() -> usize { 20 }

// ============================================================================
// VectorAgent — MCP server with all tools
// ============================================================================

#[derive(Clone)]
pub struct VectorAgent {
    core: VectorCore,
    message_buffer: Arc<Mutex<Vec<BufferedMessage>>>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl VectorAgent {
    pub fn new(core: VectorCore, message_buffer: Arc<Mutex<Vec<BufferedMessage>>>) -> Self {
        Self {
            core,
            message_buffer,
            tool_router: Self::tool_router(),
        }
    }

    // === Identity ===

    #[tool(description = "Get the current user's npub (Nostr public key in bech32 format)")]
    async fn my_npub(&self) -> Result<CallToolResult, McpError> {
        match self.core.my_npub() {
            Some(npub) => Ok(CallToolResult::success(vec![Content::text(npub)])),
            None => Ok(CallToolResult::error(vec![Content::text("Not logged in")])),
        }
    }

    // === Accounts (multi-account, GUI-parity) ===

    /// Re-attach the background DM listener to the CURRENT session, writing into the shared buffer
    /// that `get_new_messages` drains. Called after a swap once the new client is connected; the
    /// prior listener ended when `swap_session` shut its client down.
    fn respawn_listener(&self) {
        let buffer = self.message_buffer.clone();
        tokio::spawn(async move {
            let handler = Arc::new(crate::handler::AgentEventHandler::with_buffer(buffer));
            if let Err(e) = VectorCore.listen(handler).await {
                eprintln!("[vector-agent] re-listen error: {}", e);
            }
        });
    }

    #[tool(description = "List the Vector accounts this agent holds locally (each a separate npub with its own store). The active account is flagged. Use swap_account to switch between them.")]
    async fn list_accounts(&self) -> Result<CallToolResult, McpError> {
        let current = self.core.my_npub();
        match vector_core::db::get_accounts() {
            Ok(accts) => {
                let list: Vec<_> = accts.into_iter().map(|npub| {
                    let active = current.as_deref() == Some(npub.as_str());
                    serde_json::json!({ "npub": npub, "active": active })
                }).collect();
                let json = serde_json::to_string_pretty(&list).unwrap_or_else(|_| "[]".into());
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e)])),
        }
    }

    #[tool(description = "The npub of the currently active account.")]
    async fn current_account(&self) -> Result<CallToolResult, McpError> {
        match self.core.my_npub() {
            Some(npub) => Ok(CallToolResult::success(vec![Content::text(npub)])),
            None => Ok(CallToolResult::error(vec![Content::text("No active account")])),
        }
    }

    #[tool(description = "Add a Vector account and switch to it. With NO nsec, generates a fresh random identity (preferred for test accounts). With an nsec, imports it. Returns the new account's npub.")]
    async fn add_account(&self, Parameters(req): Parameters<AddAccountRequest>) -> Result<CallToolResult, McpError> {
        let nsec = match req.nsec.as_deref() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => match self.core.generate_nsec() {
                Ok(n) => n,
                Err(e) => return Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
            },
        };
        self.core.swap_session().await;
        { self.message_buffer.lock().await.clear(); }
        match self.core.login(&nsec, None).await {
            Ok(res) => {
                self.respawn_listener();
                Ok(CallToolResult::success(vec![Content::text(format!("Added and switched to {}", res.npub))]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Switch the active account to one already held locally (by npub). Loads that account's stored key — no secret key is passed or exposed. See list_accounts for options.")]
    async fn swap_account(&self, Parameters(req): Parameters<NpubRequest>) -> Result<CallToolResult, McpError> {
        let accts = vector_core::db::get_accounts().unwrap_or_default();
        if !accts.iter().any(|a| a == &req.npub) {
            return Ok(CallToolResult::error(vec![Content::text(
                format!("No local account {}. Use list_accounts or add_account first.", req.npub))]));
        }
        if self.core.my_npub().as_deref() == Some(req.npub.as_str()) {
            return Ok(CallToolResult::success(vec![Content::text(format!("Already active: {}", req.npub))]));
        }
        self.core.swap_session().await;
        // Open the target account's store to read its stored key, then bind it via the normal login path.
        if let Err(e) = vector_core::db::set_current_account(req.npub.clone()) {
            return Ok(CallToolResult::error(vec![Content::text(e)]));
        }
        if let Err(e) = vector_core::db::init_database(&req.npub) {
            return Ok(CallToolResult::error(vec![Content::text(e)]));
        }
        let nsec = match vector_core::db::get_pkey() {
            Ok(Some(n)) => n,
            Ok(None) => return Ok(CallToolResult::error(vec![Content::text(
                "That account has no stored key (encrypted or external signer) — can't swap to it headlessly.")])),
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(e)])),
        };
        { self.message_buffer.lock().await.clear(); }
        match self.core.login(&nsec, None).await {
            Ok(res) => {
                self.respawn_listener();
                Ok(CallToolResult::success(vec![Content::text(format!("Switched to {}", res.npub))]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    // === Messaging ===

    #[tool(description = "Send an encrypted direct message (NIP-17 gift-wrapped DM) to a Nostr user")]
    async fn send_dm(&self, Parameters(req): Parameters<SendDmRequest>) -> Result<CallToolResult, McpError> {
        match self.core.send_dm(&req.to_npub, &req.content).await {
            Ok(_) => Ok(CallToolResult::success(vec![Content::text(
                format!("DM sent to {}", &req.to_npub[..20.min(req.to_npub.len())])
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Send an encrypted file attachment via DM to a Nostr user")]
    async fn send_file(&self, Parameters(req): Parameters<SendFileRequest>) -> Result<CallToolResult, McpError> {
        match self.core.send_file(&req.to_npub, &req.file_path).await {
            Ok(_) => Ok(CallToolResult::success(vec![Content::text(
                format!("File sent to {}", &req.to_npub[..20.min(req.to_npub.len())])
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Get message history for a chat. Returns messages in chronological order.")]
    async fn get_messages(&self, Parameters(req): Parameters<GetMessagesRequest>) -> Result<CallToolResult, McpError> {
        let msgs = self.core.get_messages(&req.chat_id, req.limit, req.offset).await;
        let json = serde_json::to_string_pretty(&msgs).unwrap_or_else(|_| "[]".into());
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(description = "Get all new messages received since the last call. Returns and clears the incoming message buffer. Use this to poll for new DMs and group messages.")]
    async fn get_new_messages(&self) -> Result<CallToolResult, McpError> {
        let messages: Vec<BufferedMessage> = {
            let mut buf = self.message_buffer.lock().await;
            buf.drain(..).collect()
        };
        let count = messages.len();
        let json = serde_json::to_string_pretty(&messages).unwrap_or_else(|_| "[]".into());
        Ok(CallToolResult::success(vec![Content::text(
            if count == 0 { "No new messages".into() } else { json }
        )]))
    }

    #[tool(description = "List all chats (DMs and groups) with their latest message")]
    async fn list_chats(&self) -> Result<CallToolResult, McpError> {
        let chats = self.core.get_chats().await;
        let json = serde_json::to_string_pretty(&chats).unwrap_or_else(|_| "[]".into());
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(description = "Sync DM history from relays using NIP-77 negentropy reconciliation. Fetches missed messages and populates chat history. Use since_days to limit scope (e.g. 7 for last week) or omit for full sync.")]
    async fn sync_dms(&self, Parameters(req): Parameters<SyncDmsRequest>) -> Result<CallToolResult, McpError> {
        match self.core.sync_dms(req.since_days, &vector_core::NoOpEventHandler).await {
            Ok((events, new)) => Ok(CallToolResult::success(vec![Content::text(
                format!("DM sync complete: {} events processed, {} new messages", events, new)
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    // === Profiles ===

    #[tool(description = "Get a user's profile by npub (from local cache)")]
    async fn get_profile(&self, Parameters(req): Parameters<NpubRequest>) -> Result<CallToolResult, McpError> {
        match self.core.get_profile(&req.npub).await {
            Some(profile) => {
                let json = serde_json::to_string_pretty(&profile).unwrap_or_else(|_| "{}".into());
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            None => Ok(CallToolResult::error(vec![Content::text("Profile not found in cache. Use load_profile to fetch from relays.")])),
        }
    }

    #[tool(description = "Fetch a user's profile metadata from Nostr relays (updates local cache)")]
    async fn load_profile(&self, Parameters(req): Parameters<NpubRequest>) -> Result<CallToolResult, McpError> {
        if self.core.load_profile(&req.npub).await {
            match self.core.get_profile(&req.npub).await {
                Some(profile) => {
                    let json = serde_json::to_string_pretty(&profile).unwrap_or_else(|_| "{}".into());
                    Ok(CallToolResult::success(vec![Content::text(json)]))
                }
                None => Ok(CallToolResult::success(vec![Content::text("Profile fetched but not cached")])),
            }
        } else {
            Ok(CallToolResult::error(vec![Content::text("Failed to fetch profile from relays")]))
        }
    }

    #[tool(description = "Update the current user's profile (name, avatar URL, banner URL, about/bio)")]
    async fn update_profile(&self, Parameters(req): Parameters<UpdateProfileRequest>) -> Result<CallToolResult, McpError> {
        if self.core.update_profile(&req.name, &req.avatar, &req.banner, &req.about).await {
            Ok(CallToolResult::success(vec![Content::text("Profile updated")]))
        } else {
            Ok(CallToolResult::error(vec![Content::text("Failed to update profile")]))
        }
    }

    #[tool(description = "Block a user by npub")]
    async fn block_user(&self, Parameters(req): Parameters<NpubRequest>) -> Result<CallToolResult, McpError> {
        if self.core.block_user(&req.npub).await {
            Ok(CallToolResult::success(vec![Content::text(format!("Blocked {}", &req.npub[..20.min(req.npub.len())]))]))
        } else {
            Ok(CallToolResult::error(vec![Content::text("Failed to block user")]))
        }
    }

    #[tool(description = "Unblock a user by npub")]
    async fn unblock_user(&self, Parameters(req): Parameters<NpubRequest>) -> Result<CallToolResult, McpError> {
        if self.core.unblock_user(&req.npub).await {
            Ok(CallToolResult::success(vec![Content::text(format!("Unblocked {}", &req.npub[..20.min(req.npub.len())]))]))
        } else {
            Ok(CallToolResult::error(vec![Content::text("Failed to unblock user")]))
        }
    }

    #[tool(description = "Set a local nickname for a user (only visible to you)")]
    async fn set_nickname(&self, Parameters(req): Parameters<SetNicknameRequest>) -> Result<CallToolResult, McpError> {
        if self.core.set_nickname(&req.npub, &req.nickname).await {
            Ok(CallToolResult::success(vec![Content::text(format!("Nickname set to '{}'", req.nickname))]))
        } else {
            Ok(CallToolResult::error(vec![Content::text("Failed to set nickname")]))
        }
    }

    #[tool(description = "Get all blocked user profiles")]
    async fn get_blocked_users(&self) -> Result<CallToolResult, McpError> {
        let blocked = self.core.get_blocked_users().await;
        let json = serde_json::to_string_pretty(&blocked).unwrap_or_else(|_| "[]".into());
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    // === Communities ===

    #[tool(description = "List all Vector Communities held locally (owned or joined), each with its channels and channel ids. Use a channel id as the chat_id for get_messages.")]
    async fn list_communities(&self) -> Result<CallToolResult, McpError> {
        let communities = self.core.list_communities().await;
        let json = serde_json::to_string_pretty(&communities).unwrap_or_else(|_| "[]".into());
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(description = "Create a new Vector Community (single 'general' channel) owned by this identity. Signs the owner attestation so the creator is the proven owner. Returns the community + channel ids.")]
    async fn create_community(&self, Parameters(req): Parameters<CreateCommunityRequest>) -> Result<CallToolResult, McpError> {
        match self.core.create_community(&req.name).await {
            Ok(summary) => Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string_pretty(&summary).unwrap_or_else(|_| "{}".into())
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Mint a shareable public invite link for a Community this identity owns. Returns the URL.")]
    async fn create_public_invite(&self, Parameters(req): Parameters<CommunityIdRequest>) -> Result<CallToolResult, McpError> {
        match self.core.create_public_invite(&req.community_id).await {
            Ok(url) => Ok(CallToolResult::success(vec![Content::text(url)])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Send a PRIVATE community invite: gift-wrap the invite bundle directly to an npub over a NIP-17 DM (NOT a shareable link). The recipient parks it pending consent (accept_pending_invite). Requires the create-invite permission; a banned npub can't be re-invited.")]
    async fn send_private_invite(&self, Parameters(req): Parameters<CommunityMemberRequest>) -> Result<CallToolResult, McpError> {
        match self.core.invite_to_community(&req.community_id, &req.npub).await {
            Ok(v) => Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".into())
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "List the public invite links this account holds for a Community (each with its hex token, url, and expiry). Use a token with revoke_public_invite.")]
    async fn list_public_invites(&self, Parameters(req): Parameters<CommunityIdRequest>) -> Result<CallToolResult, McpError> {
        match self.core.list_public_invites(&req.community_id) {
            Ok(records) => Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string_pretty(&records).unwrap_or_else(|_| "[]".into())
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Revoke a public invite link by its hex token. Retiring the LAST active link flips the Community to Private, which re-keys (re-founds) to cut link-joined lurkers. Needs a local key when it triggers that rekey.")]
    async fn revoke_public_invite(&self, Parameters(req): Parameters<RevokePublicInviteRequest>) -> Result<CallToolResult, McpError> {
        match self.core.revoke_public_invite(&req.community_id, &req.token).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text("Revoked.")])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Join a Vector Community from a public invite URL. Fetches the invite bundle, joins, and registers its channels as chats.")]
    async fn join_community(&self, Parameters(req): Parameters<JoinCommunityRequest>) -> Result<CallToolResult, McpError> {
        match self.core.join_community(&req.invite_url).await {
            Ok(summary) => Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string_pretty(&summary).unwrap_or_else(|_| "{}".into())
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "List PRIVATE community invites received via gift-wrapped DM and parked awaiting consent (each: community_id, name, inviter_npub). Accept one with accept_pending_invite.")]
    async fn list_pending_invites(&self) -> Result<CallToolResult, McpError> {
        match self.core.list_pending_invites() {
            Ok(list) => Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string_pretty(&list).unwrap_or_else(|_| "[]".into())
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Accept a parked PRIVATE community invite by community_id (consent-then-join for a gift-wrapped invite). Folds the latest control plane, registers channels, announces presence. See list_pending_invites.")]
    async fn accept_pending_invite(&self, Parameters(req): Parameters<CommunityIdRequest>) -> Result<CallToolResult, McpError> {
        match self.core.accept_pending_invite(&req.community_id).await {
            Ok(summary) => Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string_pretty(&summary).unwrap_or_else(|_| "{}".into())
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Send a text message to a Vector Community channel. Returns the message id.")]
    async fn send_community_message(&self, Parameters(req): Parameters<SendCommunityMessageRequest>) -> Result<CallToolResult, McpError> {
        match self.core.send_community_message(&req.channel_id, &req.content, req.replied_to.as_deref()).await {
            Ok(id) => Ok(CallToolResult::success(vec![Content::text(format!("Sent (message id {id})"))])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Fetch the latest page of a Community channel from relays (messages, reactions, edits, deletes, presence). Returns the count of new messages. Then use get_messages to read them.")]
    async fn sync_community_channel(&self, Parameters(req): Parameters<SyncCommunityChannelRequest>) -> Result<CallToolResult, McpError> {
        match self.core.sync_community_channel(&req.channel_id, req.limit).await {
            Ok((n, warnings)) => {
                // Surface non-fatal warnings (catch-up / control-fold / read-cut-resume errors) so the agent
                // is never blind to "the sync ran but a re-founding couldn't be resumed."
                let mut msg = format!("Synced: {n} new message(s)");
                if !warnings.is_empty() {
                    msg.push_str("\n⚠ warnings:");
                    for w in &warnings {
                        msg.push_str(&format!("\n  - {w}"));
                    }
                }
                Ok(CallToolResult::success(vec![Content::text(msg)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "List the observed members of a Community (people who've posted or announced a join, minus those who left or are banned). Each is {npub, last_active}.")]
    async fn get_community_members(&self, Parameters(req): Parameters<CommunityIdRequest>) -> Result<CallToolResult, McpError> {
        let members = self.core.get_community_members(&req.community_id).await;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&members).unwrap_or_else(|_| "[]".into())
        )]))
    }

    #[tool(description = "Leave a Community: announces a 'left' presence, then drops the held keys and local channels. A fresh invite is needed to rejoin.")]
    async fn leave_community(&self, Parameters(req): Parameters<CommunityIdRequest>) -> Result<CallToolResult, McpError> {
        match self.core.leave_community(&req.community_id).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text("Left the community.")])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "My management capabilities in a Community (manage_metadata, kick, ban, manage_roles, etc.). Use to confirm a promotion/demotion landed. Sync the channel first so the roster is current.")]
    async fn get_community_capabilities(&self, Parameters(req): Parameters<CommunityIdRequest>) -> Result<CallToolResult, McpError> {
        match self.core.community_capabilities(&req.community_id) {
            Ok(v) => Ok(CallToolResult::success(vec![Content::text(serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".into()))])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "The Community's roles: the owner npub and the list of admin npubs. Sync the channel first so the roster is current.")]
    async fn get_community_roles(&self, Parameters(req): Parameters<CommunityIdRequest>) -> Result<CallToolResult, McpError> {
        match self.core.community_roles(&req.community_id) {
            Ok(v) => Ok(CallToolResult::success(vec![Content::text(serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".into()))])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Grant a member the Community @admin role. Requires manage_roles + outranking the role. Re-verified by every peer.")]
    async fn grant_community_admin(&self, Parameters(req): Parameters<CommunityMemberRequest>) -> Result<CallToolResult, McpError> {
        match self.core.grant_admin(&req.community_id, &req.npub).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(format!("Granted @admin to {}", req.npub))])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Revoke a member's Community @admin role.")]
    async fn revoke_community_admin(&self, Parameters(req): Parameters<CommunityMemberRequest>) -> Result<CallToolResult, McpError> {
        match self.core.revoke_admin(&req.community_id, &req.npub).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(format!("Revoked @admin from {}", req.npub))])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Kick a member (cooperative): they self-remove but can rejoin with a fresh invite. Requires the kick permission + outranking the target.")]
    async fn kick_community_member(&self, Parameters(req): Parameters<CommunityMemberRequest>) -> Result<CallToolResult, McpError> {
        match self.core.kick_member(&req.community_id, &req.npub).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(format!("Kicked {}", req.npub))])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Ban a member: terminal removal (no rejoin). In a PRIVATE community this also re-keys to cut their read access (needs a local key). Requires the ban permission + outranking the target.")]
    async fn ban_community_member(&self, Parameters(req): Parameters<CommunityMemberRequest>) -> Result<CallToolResult, McpError> {
        match self.core.set_member_banned(&req.community_id, &req.npub, true).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(format!("Banned {}", req.npub))])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Unban a member (removes them from the banlist so they may rejoin).")]
    async fn unban_community_member(&self, Parameters(req): Parameters<CommunityMemberRequest>) -> Result<CallToolResult, McpError> {
        match self.core.set_member_banned(&req.community_id, &req.npub, false).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(format!("Unbanned {}", req.npub))])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "DESTRUCTIVE, OWNER-ONLY, IRREVERSIBLE: dissolve (permanently delete) a Community. Publishes a terminal tombstone so no new messages or changes are EVER accepted by anyone (including you), and retires your own invite links. Cannot be undone. Already-sent messages aren't erased, but people can still delete their OWN past messages. Requires you to be the proven owner.")]
    async fn delete_community(&self, Parameters(req): Parameters<CommunityIdRequest>) -> Result<CallToolResult, McpError> {
        match self.core.dissolve_community(&req.community_id).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text("Community dissolved (deleted). It is permanently sealed: no new activity will be accepted.")])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Edit a Community's name and/or description (requires the manage-metadata permission). Omit a field to leave it unchanged; an empty description clears it.")]
    async fn edit_community_metadata(&self, Parameters(req): Parameters<EditCommunityMetadataRequest>) -> Result<CallToolResult, McpError> {
        match self.core.edit_community_metadata(&req.community_id, req.name.as_deref(), req.description.as_deref()).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text("Community metadata updated.")])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }
}

#[tool_handler]
impl ServerHandler for VectorAgent {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_server_info(Implementation::new("vector-agent", env!("CARGO_PKG_VERSION")))
    }
}
