### Community roles: the role list, and one role's pages.

## Sections. Each section's .keys are the words a search finds it by, separated by spaces.

roles-section-role-list = Role List
    .keys = roles role list order rank create permissions
roles-section-everyone-permissions = Permissions
    .keys = everyone role permissions ban kick manage
roles-section-role-display = Display
    .keys = role name colour color access channel scope
roles-section-role-permissions = Permissions
    .keys = role permissions ban kick manage
roles-section-role-members = Members
    .keys = role members holders assign give

## Permissions a role can hold. Each has a name and, in .desc, what it allows.

roles-perm-manage-community = Manage Community
    .desc = Change the community's name, icon and description.
roles-perm-manage-channels = Manage Channels
    .desc = Create, rename and delete channels.
roles-perm-manage-roles = Manage Roles
    .desc = Create and edit roles beneath their own, and give them to members.
roles-perm-create-invites = Create Invites
    .desc = Make invite links and invite people directly.
roles-perm-view-audit-log = View Audit Log
    .desc = See the record of moderation and management actions.
roles-perm-kick = Kick Members
    .desc = Remove members beneath them. A kicked member can rejoin.
roles-perm-ban = Ban Members
    .desc = Ban members beneath them, silencing them and cutting them off for good.
roles-perm-manage-messages = Manage Messages
    .desc = Hide messages from members beneath them.
roles-perm-pin = Pin Messages
    .desc = Pin messages to a channel for everyone to see.
roles-perm-timeout = Timeout Members
    .desc = Silence members beneath them for a while.
roles-perm-manage-nicknames = Manage Nicknames
    .desc = Change the nicknames of members beneath them.
roles-perm-view-channels = View Channels
    .desc = See channels and read what is posted in them.
roles-perm-send = Send Messages
    .desc = Post in channels.
roles-perm-threads = Send in Threads
    .desc = Reply in threads.
roles-perm-attach = Attach Files
    .desc = Share images, videos and files.
roles-perm-react = Add Reactions
    .desc = React to messages.
roles-perm-history = Read Message History
    .desc = Read what was posted before they could see a channel.
roles-perm-connect = Connect
    .desc = Join calls.
roles-perm-speak = Speak
    .desc = Talk in calls.
roles-perm-video = Video
    .desc = Share their camera or screen in calls.
roles-perm-priority = Priority Speaker
    .desc = Be heard over others while they talk.
roles-perm-mute = Mute Members
    .desc = Mute members beneath them in calls.
roles-perm-deafen = Deafen Members
    .desc = Deafen members beneath them in calls.
roles-perm-move = Move Members
    .desc = Move members beneath them between calls.
roles-perm-mention-everyone = Mention @everyone
    .desc = Notify everyone at once, and mention any role.
roles-perm-change-nickname = Change Nickname
    .desc = Choose what they go by in this community.

## The groups the permissions are listed under

roles-group-community = Community
roles-group-moderation = Moderation
roles-group-channels = Channels
roles-group-voice = Voice
roles-group-chat = Chat

## The role list

roles-title = Roles
roles-loading = Loading...
# Stands in for a channel whose name isn't known
roles-private-channel = a private channel
# Marks the community's owner at the top of the role list
roles-owner = Owner
roles-none = No roles yet. Create one to start giving members permissions.
roles-unnamed = Unnamed role
# $count is how many members hold the role.
roles-held = { $count ->
    [one] { $count } member
   *[other] { $count } members
}
# Beside @everyone, in place of a count
roles-held-everyone = all members
roles-upgrade-title = New roles are available
roles-upgrade-text = @everyone, channel and voice permissions, and roles you can delete. Every role carries over, and private channels keep who can read them.
roles-upgrade = Switch
roles-create = Create Role
roles-lede = Members get the permissions of every role they hold. A role can manage only the roles beneath it, so the order matters: drag to arrange.
# $count is how many roles the community has, $max the most it can.
roles-count = { $max ->
    [one] { $count } of { $max } role
   *[other] { $count } of { $max } roles
}

## One role's pages

roles-back = All Roles
roles-everyone-lede = What every member holds before any role. A role can add to it, never take from it.
roles-custom-colour = Custom colour
# The preview of a role that has no name yet
roles-preview-unnamed = new role
roles-new = New Role
roles-read-only = This role sits at or above your own, so you can view it but not change it.
roles-name = Role Name
roles-colour = Role Colour
roles-display-separately = Show members separately
    .desc = List its members under the role's name in the member list.
roles-mentionable = Allow anyone to mention this role
    .desc = Otherwise only members who can mention @everyone notify it.
roles-access = Access
roles-access-every-channel = Every channel
# A choice scoping a new role to one private channel. $channel is its name, after its # sign.
roles-access-only = #{ $channel } only
roles-access-hint = A role scoped to a private channel is that channel's access list: holding it is what lets someone read there. This can't be changed once the role exists.
# $channel is the private channel's name, after its # sign.
roles-access-scoped = #{ $channel } only. Holding this role is what lets someone read that channel.
roles-access-everywhere = Every channel.
roles-permissions-lede = You can only turn on permissions you hold yourself.
roles-all-hold = Everyone already holds this role.
roles-no-matches = No matches.
roles-cancel = Cancel
roles-adding = Adding...
roles-add = Add
# $count is how many picked members the button adds.
roles-add-count = Add { $count }
roles-nobody = Nobody holds this role yet.
roles-remove = Remove from role
roles-members = Members
roles-add-members = Add Members
roles-delete = Delete Role

## Deleting a role, and switching a community to the new roles

# A new role's name until it's given one.
roles-new-name = new role
# Stands in for a role with no name in roles-delete-text.
roles-this-role = this role
roles-delete-title = Delete role
# $name is the role's name; $holders is how many members hold it.
roles-delete-text = { $holders ->
    [0] Delete { $name }? This can't be undone.
    [one] Delete { $name }? The member who holds it loses its permissions. This can't be undone.
   *[other] Delete { $name }? The { $holders } members who hold it lose its permissions. This can't be undone.
}
roles-switch-title = Switch to the new roles
roles-switch-text = Adds @everyone, channel permissions and role deletion. Every role carries over. Members on an older Vector keep the roles from before the switch and won't see later changes until they update.
roles-switch = Switch
roles-name-required = A role needs a name
roles-change-failed = The roles couldn't be changed
roles-holders-failed = Couldn't change who holds this role
