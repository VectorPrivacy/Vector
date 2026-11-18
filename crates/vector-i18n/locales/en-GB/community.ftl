### A community's own view: its banner, channel list and member list.

community-hide-banner = Hide Banner
community-show-banner = Show Banner

## The community's menu

community-invite = Invite People
community-nickname = Change Nickname
community-settings = Community Settings
community-moderation = Moderation
# Beside Moderation during a raid. $count is how many members are flagged.
community-flagged = { $count } flagged
community-delete = Delete Community
community-leave = Leave Community

## A channel's or category's menu

channel-menu-delete = Delete Channel
channel-menu-edit-category = Edit Category
channel-menu-delete-category = Delete Category

## Leaving or deleting a community

community-delete-title = Delete this community?
# <b> is bold, <br> a line break. $name is the community's name.
community-delete-text = This permanently ends "<b>{ $name }</b>" for everyone, including you. No new messages can be sent and no one can rejoin. People can still delete their own past messages. This cannot be undone.<br><br>Type the community name to confirm:
# <b> is bold. $name is the community's name.
community-leave-text = Leave "<b>{ $name }</b>"? You'll need a new invite to rejoin.

## A member posting without a membership on record, on clicking their mark

community-departed-title = Departed Member
community-departed-text = They left or were removed, yet are still posting. This may be suspicious, keep an eye on them.
community-unannounced-title = Unannounced Member
community-unannounced-text = They joined and are posting without ever publishing a join event. This may be suspicious, keep an eye on them.

# A private channel whose name this device can't read.
community-private-channel = private channel
# The community name field's and description field's placeholders.
community-name-placeholder = Community name
community-description-placeholder = Tell people what brings everyone here

## Setting a nickname in a community

community-nickname-title = Change Nickname
# $name is the member's name.
community-nickname-title-other = Change { $name }'s Nickname
community-nickname-placeholder = Nickname
community-nickname-text-mine = Everyone in this community sees it. Leave it empty to go by your own name.
community-nickname-text-theirs = Everyone in this community sees it. Leave it empty to go by their own name.
community-nickname-failed = Couldn't set the nickname

## Joining

community-invalid-invite = Invalid Invite
community-invalid-invite-text = This invite link could not be loaded.
# $error is why, as the relays gave it.
community-invalid-invite-why = This invite link could not be loaded.

    { $error }
community-invite-loading = Loading community invite…
# $name is the community's name.
community-join-title = Join { $name }?
# $name is the community's name.
community-join-text = You've been invited to join { $name }.
# $name is the community's name.
community-joining = Joining { $name }…
community-join-failed = Failed to Join
community-join-refused = Couldn't Join
community-removed-by-admin = You were removed from this community by an admin
community-move-failed = Couldn't move it

## Opening a go link

go-broken = That link is broken
go-not-member = You're not in this community
go-channel-unavailable = That channel isn't available to you
go-message-deleted = That message was deleted
go-message-not-yet = That message isn't on this device yet
go-message-missing = That message isn't on this device

## A channel row's buttons, and the header's

community-channel-invite = Invite People
community-channel-edit = Edit Channel
community-channel-chat = Open Chat
# $category is the category's name.
community-add-channel-to = Add a channel to { $category }
community-chat-options = Chat Options
community-details-hide = Hide Details
community-details-show = Show Details
# $count is how many members the community has.
community-member-count = { $count ->
    [one] { $count } Member
   *[other] { $count } Members
}

## The member list. Section headings are shown in capitals.

community-section-admin = Admin
community-section-members = Members
community-owner = Owner
community-bot = Bot
community-departed = Posting after leaving
community-unannounced = Never announced joining
community-members-loading = Loading members…
community-members-quiet = No one has spoken yet. Members appear here once they post.
community-members-no-matches = No matches.

## An invite to a community

community-invite-card = Community invite
community-invite-accept = Accept Invite
community-invite-decline = Decline Invite
community-invite-from = From
community-invite-owner = Owner
community-invite-sent = Sent
community-invite-received = Received
community-invite-expires = Expires
community-invite-accept-short = Accept
community-invite-decline-short = Decline
