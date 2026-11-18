### A community's Audit Log: each entry as a sentence, the details it opens to, and the filter.
### Names in a sentence ($actor, $member, $role, $channel and the like) are shown in bold.

## The filter

audit-family-all = All Actions
audit-family-community = Community
audit-family-roles = Roles
audit-family-members = Members
audit-family-channels = Channels
audit-family-messages = Messages
audit-family-voice = Voice

## Names standing in for what can't be named

# The actor, when the entry doesn't say who
audit-someone = Someone
audit-deleted-role = a deleted role
audit-private-channel = a private channel
audit-unknown-category = a category
# $bit is the number of a permission this version doesn't know.
audit-permission-unknown = Permission { $bit }

## Sentences. $actor is who did it.

# $actor is who did it.
audit-community-create = { $actor } created the community
# $actor is who did it.
audit-community-update = { $actor } updated the community
# $actor is who did it.
audit-roles-activate = { $actor } switched the community to its new roles
# $actor is who did it, $role is @everyone.
audit-everyone-update = { $actor } changed what { $role } can do
# $actor is who did it, $role the role.
audit-role-create = { $actor } created the role { $role }
# $actor is who did it, $role the role.
audit-role-update = { $actor } updated the role { $role }
# $actor is who did it, $role the role.
audit-role-delete = { $actor } deleted the role { $role }
# $actor is who did it, $member who got the role, $role the role.
audit-member-role-give = { $actor } gave { $member } the role { $role }
# $actor is who did it, $role the role, $member who lost it.
audit-member-role-take = { $actor } took { $role } from { $member }
# $actor is who did it, $member whose roles changed.
audit-member-roles-update = { $actor } changed the roles of { $member }
# $actor is who did it, $member who was banned.
audit-member-ban = { $actor } banned { $member }
# $actor is who did it, $member who was unbanned.
audit-member-unban = { $actor } unbanned { $member }
# $actor is who did it.
audit-banlist-update = { $actor } updated the ban list
# $actor is who did it, $member who was kicked.
audit-member-kick = { $actor } kicked { $member }
# $actor is who did it, $member who was timed out.
audit-member-timeout = { $actor } timed out { $member }
# $actor is who did it, $member who was timed out, $until the date and time it ends.
audit-member-timeout-until = { $actor } timed out { $member } until { $until }
# $actor is who did it, $member whose timeout ended.
audit-member-timeout-remove = { $actor } lifted the timeout on { $member }
# $actor is who did it, $nick the nickname.
audit-nick-set-own = { $actor } set their nickname to { $nick }
# $actor is who did it, $member whose nickname it is, $nick the nickname.
audit-nick-set = { $actor } set the nickname of { $member } to { $nick }
# $actor is who did it.
audit-nick-remove-own = { $actor } removed their nickname
# $actor is who did it, $member whose nickname it was.
audit-nick-remove = { $actor } removed the nickname of { $member }
# $actor is who did it, $channel the channel.
audit-channel-create = { $actor } created the channel { $channel }
# $actor is who did it, $channel the channel.
audit-channel-create-private = { $actor } created the private channel { $channel }
# $actor is who did it, $channel the channel.
audit-channel-create-voice = { $actor } created the voice channel { $channel }
# $actor is who did it, $channel the channel.
audit-channel-create-private-voice = { $actor } created the private voice channel { $channel }
# $actor is who did it, $channel the channel.
audit-channel-update = { $actor } updated { $channel }
# $actor is who did it, $channel the channel.
audit-channel-delete = { $actor } deleted the channel { $channel }
# $actor is who did it, $category the category.
audit-category-create = { $actor } created the category { $category }
# $actor is who did it, $category the category.
audit-category-update = { $actor } updated the category { $category }
# $actor is who did it, $category the category.
audit-category-delete = { $actor } deleted the category { $category }
# $actor is who did it, $target the role or member, $channel the channel or category.
audit-overwrite-create = { $actor } set permissions for { $target } in { $channel }
# $actor is who did it, $target the role or member, $channel the channel or category.
audit-overwrite-update = { $actor } changed permissions for { $target } in { $channel }
# $actor is who did it, $target the role or member, $channel the channel or category.
audit-overwrite-delete = { $actor } cleared the permissions for { $target } in { $channel }
# $actor is who did it, $channel the channel, $category its category.
audit-channel-sync = { $actor } synced { $channel } to { $category }
# $actor is who did it, $channel the channel.
audit-channel-unsync = { $actor } unsynced { $channel } from its category
# $actor is who did it, $author who wrote the message, $channel where it is.
audit-message-pin = { $actor } pinned a message by { $author } in { $channel }
# $actor is who did it, $author who wrote the message, $channel where it is.
audit-message-unpin = { $actor } unpinned a message by { $author } in { $channel }
# $actor is who did it, $channel the channel.
audit-pins-update = { $actor } changed the pins in { $channel }
# $actor is who did it, $author who wrote the message, $channel where it was.
audit-message-delete = { $actor } deleted a message by { $author } in { $channel }
# $actor is who did it, $count how many links.
audit-invite-create = { $count ->
    [one] { $actor } created an invite link
   *[other] { $actor } created { $count } invite links
}
# $actor is who did it, $count how many links.
audit-invite-delete = { $count ->
    [one] { $actor } revoked an invite link
   *[other] { $actor } revoked { $count } invite links
}
# $actor is who did it.
audit-keys-lockdown = { $actor } locked the community down and rotated its keys
# $actor is who did it.
audit-keys-rotate = { $actor } rotated the community's keys
# $actor is who did it, $member who was muted, $channel the call.
audit-voice-mute = { $actor } muted { $member } in { $channel }
# $actor is who did it, $member who was unmuted, $channel the call.
audit-voice-unmute = { $actor } unmuted { $member } in { $channel }
# $actor is who did it, $member who was deafened, $channel the call.
audit-voice-deafen = { $actor } deafened { $member } in { $channel }
# $actor is who did it, $member who was undeafened, $channel the call.
audit-voice-undeafen = { $actor } undeafened { $member } in { $channel }
# $actor is who did it, $member who was moved, $channel the call they went to.
audit-voice-move = { $actor } moved { $member } to { $channel }
# $actor is who did it, $member who was disconnected, $channel the call.
audit-voice-disconnect = { $actor } disconnected { $member } from { $channel }
# $actor is who did it, for an action this version doesn't know.
audit-unknown = { $actor } made a change

## Details, one line each

audit-view-chat = Text
audit-view-voice = Voice
audit-view-forum = Forum
# A role's colour when it has none
audit-colour-none = none
# $old is the name before, $new after.
audit-detail-rename = Name: { $old } → { $new }
# $new is the name given.
audit-detail-name = Name: { $new }
audit-detail-description = Changed the description
audit-detail-description-removed = Removed the description
# $topic is the new topic.
audit-detail-topic = Topic: { $topic }
audit-detail-topic-removed = Removed the topic
audit-detail-icon-added = Added the icon
audit-detail-icon-removed = Removed the icon
audit-detail-icon-changed = Changed the icon
audit-detail-banner-added = Added the banner
audit-detail-banner-removed = Removed the banner
audit-detail-banner-changed = Changed the banner
# $channel is where new members start.
audit-detail-landing = New members start in { $channel }
audit-detail-landing-default = New members start in the default channel
audit-detail-system-off = Turned off join and leave messages
# $channel is where the messages go.
audit-detail-system = Join and leave messages go to { $channel }
audit-detail-system-default = Join and leave messages go to the default channel
# $relay is the relay's address.
audit-detail-relay-added = Added the relay { $relay }
# $relay is the relay's address.
audit-detail-relay-removed = Removed the relay { $relay }
# $relay is the relay's address.
audit-detail-voice-relay-added = Added the voice relay { $relay }
# $relay is the relay's address.
audit-detail-voice-relay-removed = Removed the voice relay { $relay }
# $old and $new are colours, such as #59fcb3.
audit-detail-recolour = Colour: { $old } → { $new }
# $new is a colour, such as #59fcb3.
audit-detail-colour = Colour: { $new }
# A new role's permissions. $list names them, separated by commas.
audit-detail-permissions = Permissions: { $list }
# $list names the permissions, separated by commas.
audit-detail-granted = Granted: { $list }
# $list names the permissions, separated by commas.
audit-detail-revoked = Revoked: { $list }
# $list names the permissions, separated by commas.
audit-detail-allowed = Allowed: { $list }
# $list names the permissions, separated by commas.
audit-detail-unallowed = No longer allowed: { $list }
# $list names the permissions, separated by commas.
audit-detail-denied = Denied: { $list }
# $list names the permissions, separated by commas.
audit-detail-undenied = No longer denied: { $list }
audit-detail-position = Moved it in the list
audit-detail-display = Shown on its own in the member list
audit-detail-display-off = No longer shown on its own in the member list
audit-detail-mentionable = Anyone can mention it
audit-detail-mentionable-off = No longer mentionable by everyone
# $old is the channel's type before, $new after: Text, Voice or Forum.
audit-detail-view = Type: { $old } → { $new }
audit-detail-private = Made it private
# $category is the category it moved to.
audit-detail-moved = Moved to { $category }
audit-detail-uncategorised = Moved out of its category
# $role is the role given.
audit-detail-role-added = Added { $role }
# $role is the role taken.
audit-detail-role-removed = Removed { $role }
audit-detail-no-roles = Holds no roles
# $roles names every role the member holds, separated by commas.
audit-detail-roles = Holds { $roles }
# $old is the nickname before, $new after.
audit-detail-renick = Nickname: { $old } → { $new }
# $old is the nickname that was removed.
audit-detail-nick-removed = Was { $old }
audit-detail-no-bans = Nobody is banned
# $members names everyone banned, separated by commas.
audit-detail-bans = Banned: { $members }
audit-detail-partial = What came before this was compacted when the community's keys were rotated, so what it changed isn't known.

## When an entry happened

audit-just-now = Just now
# $count is how many minutes ago.
audit-minutes-ago = { $count ->
    [one] A minute ago
   *[other] { $count } minutes ago
}
# $time is the time of day.
audit-today-at = Today at { $time }
# $time is the time of day.
audit-yesterday-at = Yesterday at { $time }
