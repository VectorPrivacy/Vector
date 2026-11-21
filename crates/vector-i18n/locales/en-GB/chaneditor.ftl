### A channel's or category's editor: its pages and their sections.

## Pages

channel-editor-page-overview = Overview
channel-editor-page-permissions = Permissions

## Sections inside the pages. Each section's .keys are the words a search finds it by, separated by spaces.

channel-editor-section-name = Name
    .keys = name rename title
channel-editor-section-topic = Topic
    .keys = topic description about
channel-editor-section-view = Opens As
    .keys = view text voice opens
channel-editor-section-private = Privacy
    .keys = private privacy public hidden key
channel-editor-section-sync = Category Sync
    .keys = sync category inherit
channel-editor-section-lock = Lockdown
    .keys = lock lockdown locked spam read only announcements
channel-editor-section-targets = Roles & Members
    .keys = roles members everyone overwrite who
channel-editor-section-perm-channel = Channel
    .keys = view send threads attach files reactions history manage messages hide
channel-editor-section-perm-voice = Voice
    .keys = connect speak video priority mute deafen move timeout

## Permissions. Each has a name and, in .desc, what it allows.

channel-editor-perm-view = View Channel
    .desc = See the channel and read it. A private channel's key follows this.
channel-editor-perm-send = Send Messages
    .desc = Post here. Without it for @everyone, only those you allow can post.
channel-editor-perm-threads = Send in Threads
    .desc = Reply inside a thread.
channel-editor-perm-attach = Attach Files
    .desc = Share images, videos and files.
channel-editor-perm-react = Add Reactions
    .desc = React to messages.
channel-editor-perm-history = Read Message History
    .desc = Read what was said before they could see the channel.
channel-editor-perm-manage = Manage Messages
    .desc = Hide messages from members beneath them here.
channel-editor-perm-connect = Connect
    .desc = Join the channel's call.
channel-editor-perm-speak = Speak
    .desc = Be heard in the call.
channel-editor-perm-video = Video
    .desc = Share a camera or a screen.
channel-editor-perm-priority = Priority Speaker
    .desc = Lower others while speaking.
channel-editor-perm-mute = Mute Members
    .desc = Mute others in the call.
channel-editor-perm-deafen = Deafen Members
    .desc = Stop others hearing the call.
channel-editor-perm-move = Move Members
    .desc = Move others between calls.
channel-editor-perm-timeout = Timeout Members
    .desc = Time out members beneath them here.
channel-editor-perm-closed = A private channel opens only to the roles and members you allow.

## Overview

channel-editor-view-chat = Text
    .desc = Messages first.
channel-editor-view-voice = Voice
    .desc = Opens its call, with the chat beside it.
channel-editor-category-name = Category Name
channel-editor-channel-name = Channel Name
channel-editor-topic-hint = What this channel is for
channel-editor-opens-as-lede = How a member's client opens the channel. It changes nothing about who can do what.
channel-editor-private = Private Channel
channel-editor-private-voice = Only members and roles you let in can join it.
channel-editor-private-text = Only members and roles you let in can see it.

## Permissions page

channel-editor-loading = Loading...
channel-editor-needs-manage-roles = Changing who can do what here needs Manage Roles.
channel-editor-not-activated = Channel permissions come with the new roles. Switch to them in Community Settings, under Roles.
channel-editor-synced = This channel follows its category's permissions. Edit them from the category, or stop syncing to set its own.
channel-editor-synced-held = This channel follows its category's permissions. Edit them from the category, or stop syncing to set its own. Stopping would change rules set above your roles, so only someone above you can.
channel-editor-own = This channel sets its own permissions.
channel-editor-own-held = This channel sets its own permissions. Syncing would replace rules set above your roles, so only someone above you can.
channel-editor-stop-sync = Stop syncing
channel-editor-sync = Sync with its category
channel-editor-locked = Locked. Only the roles and members you allow can post, react or join calls here.
channel-editor-lock-note = Only the roles and members you allow can post, react or join calls, and nothing anyone else sends reaches its readers.
channel-editor-unlock = Unlock
channel-editor-lock-category = Lock Category
channel-editor-lock-channel = Lock Channel
channel-editor-add = Add
channel-editor-no-rules = no rules yet
channel-editor-cant-change = You can't change these rules
channel-editor-category-targets = Channels synced to this category follow these rules.
channel-editor-targets = Pick who to set rules for. Anything left alone follows what they hold across the community.
# Beside a permissions heading. $target is the role or member whose rules show.
channel-editor-for = for { $target }
channel-editor-roles = Roles
channel-editor-members = Members
# $count is how many more members match than the list shows.
channel-editor-search-more = Search to find the other { $count }.
# $query is what was typed into the search.
channel-editor-no-match = Nobody matches "{ $query }".
channel-editor-deleted-role = Deleted role
channel-editor-why-support = Its rules here include permissions your roles don't give you, so only someone above you can change them.
channel-editor-why-everyone = Changing @everyone here needs Manage Roles from one of your roles.
channel-editor-why-self = These are your own rules here, so only someone above you can change them.
channel-editor-why-member = This member holds a role at or above yours, so you can see their rules here but not change them.
channel-editor-why-role = This role sits at or above your own, so you can see its rules here but not change them.

## The save bar and the footer

channel-editor-saving = Saving...
# $target is the role or member whose rules are open.
channel-editor-save-first = Save or reset the rules for { $target } first
channel-editor-save-first-unnamed = Save or reset the rules for this target first
channel-editor-save-before-closing = Save or reset your changes before closing
channel-editor-unsaved = You have unsaved changes
channel-editor-reset = Reset
channel-editor-save = Save Changes
channel-editor-category = Category
channel-editor-channel = Channel
channel-editor-delete-category = Delete Category
channel-editor-delete-channel = Delete Channel

## Placeholders

channel-editor-topic-placeholder = Shown at the top of the channel
channel-editor-find-target = Find a role or member

## Deleting

channel-editor-delete-category-title = Delete category
channel-editor-delete-channel-title = Delete channel
# $name is the category's name.
channel-editor-delete-category-text = Delete <b>{ $name }</b>? Its channels stay, uncategorized.
# $name is the text channel's name, after its # sign.
channel-editor-delete-text-channel-text = Delete <b>#{ $name }</b>? It's deleted for everyone, with all its messages and files.
# $name is the voice channel's name.
channel-editor-delete-voice-channel-text = Delete <b>{ $name }</b>? It's deleted for everyone, with all its messages and files.
channel-editor-delete-category-failed = Couldn't delete the category
channel-editor-delete-channel-failed = Couldn't delete the channel
