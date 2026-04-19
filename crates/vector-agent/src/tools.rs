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
pub struct SendGroupMessageRequest {
    #[schemars(description = "Group ID (64-char hex)")]
    pub group_id: String,
    #[schemars(description = "Message content")]
    pub content: String,
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
pub struct CreateGroupRequest {
    #[schemars(description = "Group name")]
    pub name: String,
    #[schemars(description = "Members to invite: array of {npub, device_id} objects")]
    #[serde(default)]
    pub members: Vec<MemberDevice>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MemberDevice {
    #[schemars(description = "Member's npub")]
    pub npub: String,
    #[schemars(description = "Member's device ID")]
    pub device_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GroupIdRequest {
    #[schemars(description = "Group ID (64-char hex)")]
    pub group_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct WelcomeIdRequest {
    #[schemars(description = "Welcome event ID (from list_invites)")]
    pub welcome_event_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NpubGroupRequest {
    #[schemars(description = "Group ID (64-char hex)")]
    pub group_id: String,
    #[schemars(description = "Member's npub")]
    pub npub: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct InviteMemberRequest {
    #[schemars(description = "Group ID")]
    pub group_id: String,
    #[schemars(description = "Member's npub")]
    pub npub: String,
    #[schemars(description = "Member's device ID")]
    pub device_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct InviteMembersRequest {
    #[schemars(description = "Group ID")]
    pub group_id: String,
    #[schemars(description = "Members to invite: array of {npub, device_id} objects")]
    pub members: Vec<MemberDevice>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RemoveMemberRequest {
    #[schemars(description = "Group ID")]
    pub group_id: String,
    #[schemars(description = "Member's npub to remove")]
    pub npub: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdateGroupRequest {
    #[schemars(description = "Group ID")]
    pub group_id: String,
    #[schemars(description = "New group name (optional)")]
    pub name: Option<String>,
    #[schemars(description = "New group description (optional)")]
    pub description: Option<String>,
    #[schemars(description = "New admin npubs list (optional)")]
    pub admin_npubs: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NpubRequest {
    #[schemars(description = "User's npub")]
    pub npub: String,
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

    #[tool(description = "Send a message to an MLS-encrypted group chat")]
    async fn send_group_message(&self, Parameters(req): Parameters<SendGroupMessageRequest>) -> Result<CallToolResult, McpError> {
        match self.core.send_group_message(&req.group_id, &req.content).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text("Group message sent")])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Get message history for a chat (DM or group). Returns messages in chronological order.")]
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

    // === Groups ===

    #[tool(description = "Create a new MLS-encrypted group chat. Returns the group_id. Members are optional — you can invite later.")]
    async fn create_group(&self, Parameters(req): Parameters<CreateGroupRequest>) -> Result<CallToolResult, McpError> {
        let devices: Vec<(&str, &str)> = req.members.iter()
            .map(|m| (m.npub.as_str(), m.device_id.as_str()))
            .collect();
        match self.core.create_group(&req.name, &devices).await {
            Ok(group_id) => Ok(CallToolResult::success(vec![Content::text(
                format!("Group created: {}", group_id)
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "List all MLS groups you are a member of")]
    async fn list_groups(&self) -> Result<CallToolResult, McpError> {
        match self.core.list_groups().await {
            Ok(groups) => {
                let json = serde_json::to_string_pretty(&groups).unwrap_or_else(|_| "[]".into());
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Get the members and admins of an MLS group")]
    async fn get_group_members(&self, Parameters(req): Parameters<GroupIdRequest>) -> Result<CallToolResult, McpError> {
        match self.core.get_group_members(&req.group_id) {
            Ok((group_id, members, admins)) => {
                let result = serde_json::json!({
                    "group_id": group_id,
                    "members": members,
                    "admins": admins,
                });
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&result).unwrap()
                )]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Invite a single member to an MLS group by npub (auto-fetches their latest keypackage). Prefer this over invite_member_with_device.")]
    async fn invite(&self, Parameters(req): Parameters<NpubGroupRequest>) -> Result<CallToolResult, McpError> {
        match self.core.invite(&req.group_id, &req.npub).await {
            Ok(device_id) => Ok(CallToolResult::success(vec![Content::text(
                format!("Invited {} (device {})", &req.npub[..20.min(req.npub.len())], &device_id[..8.min(device_id.len())])
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Fetch a user's published MLS keypackages from relays. Returns list of (device_id, created_at). Advanced — prefer `invite` which handles this automatically.")]
    async fn fetch_keypackages(&self, Parameters(req): Parameters<NpubRequest>) -> Result<CallToolResult, McpError> {
        match self.core.fetch_keypackages(&req.npub).await {
            Ok(packages) => {
                let json = serde_json::to_string_pretty(&packages.iter()
                    .map(|(id, ts)| serde_json::json!({"device_id": id, "created_at": ts}))
                    .collect::<Vec<_>>()
                ).unwrap_or_else(|_| "[]".into());
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Invite a single member to an MLS group with explicit device_id. Advanced — prefer `invite` which auto-selects latest keypackage.")]
    async fn invite_member(&self, Parameters(req): Parameters<InviteMemberRequest>) -> Result<CallToolResult, McpError> {
        match self.core.invite_member(&req.group_id, &req.npub, &req.device_id).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(
                format!("Invited {} to group", &req.npub[..20.min(req.npub.len())])
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Invite multiple members to an MLS group in a single commit")]
    async fn invite_members(&self, Parameters(req): Parameters<InviteMembersRequest>) -> Result<CallToolResult, McpError> {
        let devices: Vec<(&str, &str)> = req.members.iter()
            .map(|m| (m.npub.as_str(), m.device_id.as_str()))
            .collect();
        match self.core.invite_members(&req.group_id, &devices).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(
                format!("Invited {} members to group", req.members.len())
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Remove a member from an MLS group (admin only)")]
    async fn remove_member(&self, Parameters(req): Parameters<RemoveMemberRequest>) -> Result<CallToolResult, McpError> {
        match self.core.remove_member(&req.group_id, &req.npub).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(
                format!("Removed {} from group", &req.npub[..20.min(req.npub.len())])
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Update MLS group metadata (name, description, or admin list)")]
    async fn update_group(&self, Parameters(req): Parameters<UpdateGroupRequest>) -> Result<CallToolResult, McpError> {
        let admin_refs: Option<Vec<&str>> = req.admin_npubs.as_ref()
            .map(|v| v.iter().map(|s| s.as_str()).collect());
        match self.core.update_group(
            &req.group_id,
            req.name.as_deref(),
            req.description.as_deref(),
            admin_refs.as_deref(),
        ).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text("Group updated")])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Leave an MLS group")]
    async fn leave_group(&self, Parameters(req): Parameters<GroupIdRequest>) -> Result<CallToolResult, McpError> {
        match self.core.leave_group(&req.group_id).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text("Left group")])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Publish this device's MLS KeyPackage to relays. Required before anyone can invite you to MLS groups. Called automatically on agent startup — only use manually if you suspect your keypackage is missing or corrupted.")]
    async fn publish_keypackage(&self) -> Result<CallToolResult, McpError> {
        match self.core.publish_keypackage(false).await {
            Ok(kp) => Ok(CallToolResult::success(vec![Content::text(
                format!("KeyPackage published: device={}, ref={}", kp.device_id, kp.keypackage_ref)
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "List pending MLS group invites that you've received but not yet accepted. Each invite has a welcome_event_id used for accept/decline.")]
    async fn list_invites(&self) -> Result<CallToolResult, McpError> {
        match self.core.list_invites().await {
            Ok(invites) => {
                let json = serde_json::to_string_pretty(&invites).unwrap_or_else(|_| "[]".into());
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Accept a pending MLS group invite by its welcome_event_id. Joins the group, syncs participants, and fetches recent messages.")]
    async fn accept_invite(&self, Parameters(req): Parameters<WelcomeIdRequest>) -> Result<CallToolResult, McpError> {
        match self.core.accept_invite(&req.welcome_event_id).await {
            Ok(group_id) => Ok(CallToolResult::success(vec![Content::text(
                format!("Joined group: {}", group_id)
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Decline a pending MLS group invite by its welcome_event_id. Removes it without joining.")]
    async fn decline_invite(&self, Parameters(req): Parameters<WelcomeIdRequest>) -> Result<CallToolResult, McpError> {
        match self.core.decline_invite(&req.welcome_event_id).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text("Invite declined")])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(description = "Sync all MLS groups from relays. Returns count of processed events and new messages.")]
    async fn sync_groups(&self) -> Result<CallToolResult, McpError> {
        match self.core.sync_groups().await {
            Ok((processed, new)) => Ok(CallToolResult::success(vec![Content::text(
                format!("Synced: {} events processed, {} new messages", processed, new)
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
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
