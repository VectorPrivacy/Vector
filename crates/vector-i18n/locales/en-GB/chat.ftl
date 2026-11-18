### Vector's British English, the only language built into the app. Every other language
### mirrors these files. A message whose English meaning changes gets a new id.

## Date dividers in the message timeline

day-today = Today
day-yesterday = Yesterday

## Typing

# The chat list's preview line while the other person in a DM is typing.
typing-dm = Typing...

# The chat list's preview line while people in a group are typing.
# $count is how many are typing; $first, $second and $third are the first three names
# (empty when there are fewer).
typing-group = { $count ->
    [1] { $first } is typing...
    [2] { $first } and { $second } are typing...
    [3] { $first }, { $second }, and { $third } are typing...
   *[other] Several people are typing...
}

# The strip above the composer naming who is typing in the open chat. <b> marks a name.
# $count is how many are typing; $first, $second and $third are the first three names
# (empty when there are fewer).
typing-strip = { $count ->
    [1] <b>{ $first }</b> is typing
    [2] <b>{ $first }</b> and <b>{ $second }</b> are typing
    [3] <b>{ $first }</b>, <b>{ $second }</b> and <b>{ $third }</b> are typing
   *[other] Several people are typing
}

## Messages shown collapsed, on the button that reveals them

hidden-blocked = Blocked message
hidden-unauthorized = Unauthorized poster
hidden-no-send = Posted where they can't send
hidden-timed-out = Sent while timed out
# A run of collapsed messages in a row from one blocked person.
hidden-blocked-run = { $count ->
    [one] { $count } blocked message
   *[other] { $count } blocked messages
}
# On a revealed message, collapsing it again. $what is one of the labels above.
hidden-hide = { $what } · Hide

## Membership and chat events, as lines in the timeline

# <b> marks who did it; the line opens their profile.
system-left = <b>{ $name }</b> has left
# <b> marks who joined.
system-joined = <b>{ $name }</b> has joined
# $link is the label of the invite link they joined through.
system-joined-via = <b>{ $name }</b> has joined via { $link }
# <b> marks who was removed.
system-removed = <b>{ $name }</b> was removed
# The same event several times in a row shows as one line; $count is how many.
system-wallpaper-changed = { $count ->
    [one] <b>{ $name }</b> changed the wallpaper
   *[other] <b>{ $name }</b> changed the wallpaper { $count } times
}
# $count is how many times in a row.
system-wallpaper-removed = { $count ->
    [one] <b>{ $name }</b> removed the wallpaper
   *[other] <b>{ $name }</b> removed the wallpaper { $count } times
}
# $count is how many times in a row.
system-pins-modified = { $count ->
    [one] <b>{ $name }</b> modified the Pins
   *[other] <b>{ $name }</b> modified the Pins { $count } times
}
# Tooltip on the invite link's label in a join line. $name made the link.
system-via-tip = { $name }'s invite link

## Why the composer can't send, shown as its placeholder

compose-lock-blocked = Unblock to send messages
compose-lock-dissolved = This community has been dissolved.
compose-lock-no-permission = You can't send messages in this channel
compose-lock-key-pending = Your key to post here is on its way
# $until is a weekday and time, like "Tue 03:15 PM".
compose-lock-timed-out = You're timed out until { $until }

## What closes the timeline

chat-end-blocked = Blocked: you won't receive new messages from them
chat-end-dissolved = This community was dissolved by the owner.
# $name is the community's name.
chat-end-dissolved-named = { $name } was dissolved by the owner.

## A chat's wallpaper

wallpaper-remove-title = Remove wallpaper?
wallpaper-remove-text = This will remove the wallpaper and revert to the default theme. This change syncs to your contact and your other devices.
wallpaper-uploading = Uploading…
# $percent is how much has gone up, from 1 to 100.
wallpaper-uploading-percent = Uploading… { $percent }%

## Blocking someone from a chat

block-user-title = Block User
block-user-text = Are you sure you want to block this user? You will no longer receive DMs from them.

## Titles of the popup when a notification setting couldn't be saved

chat-notify-failed = Notification Settings
chat-mute-failed = Mute

## Jumping to the first unread message

unread-jump-syncing = Syncing…
# $percent is how much history has arrived, from 0 to 100.
unread-jump-syncing-percent = Syncing… { $percent }%
unread-jump-unreachable = Couldn't reach the first unread message.

## A custom emoji that didn't load, on clicking it

emoji-unavailable-title = Emoji unavailable
emoji-unavailable-text = This emoji couldn't be loaded (the host may be offline, or the file was removed).
# $why is the reason, as the loader gave it.
emoji-unavailable-why = This emoji couldn't be loaded ({ $why }).

## Blocking and unblocking: the toast once it's done

block-user-done = User Blocked
unblock-user-done = User Unblocked
unblock-user-title = Unblock User
# $name is who would be unblocked.
unblock-user-text = Are you sure you want to unblock { $name }?
request-accept-failed = Failed to accept
wallpaper-remove-failed = Wallpaper not removed
wallpaper-processing = Processing image...
wallpaper-bad-image = Couldn’t use that image
wallpaper-edit-mode = Edit Mode is enabled.
wallpaper-static-title = Static wallpapers only
wallpaper-static-text = Vector wallpapers don’t animate. We grabbed the first frame of your image to use instead.
wallpaper-hidden-title = Background Wallpaper
# <b> is bold. The path names the switch in Settings.
wallpaper-hidden-text = Your wallpaper is set. If you want to be able to see it, please enable <b>Settings → Display → Background Wallpaper</b>.
wallpaper-not-sent = Wallpaper not sent

## A chat's header

chat-notes = Notes
chat-notes-subtitle = Encrypted Notes to Self
chat-call = Call
chat-options = Chat Options
chat-menu-self-destruct = Self-Destruct Timer
chat-menu-edit-channel = Edit Channel
chat-menu-wallpaper-change = Change Wallpaper
chat-menu-wallpaper-remove = Remove Wallpaper

## How long new messages last, in the Self-Destruct Timer menu

self-destruct-permanent = Permanent
# $count is how many weeks.
self-destruct-weeks = { $count ->
    [one] { $count } Week
   *[other] { $count } Weeks
}
# $count is how many days.
self-destruct-days = { $count ->
    [one] { $count } Day
   *[other] { $count } Days
}
# $count is how many hours.
self-destruct-hours = { $count ->
    [one] { $count } Hour
   *[other] { $count } Hours
}
# $count is how many minutes.
self-destruct-minutes = { $count ->
    [one] { $count } Minute
   *[other] { $count } Minutes
}
# $count is how many seconds.
self-destruct-seconds = { $count ->
    [one] { $count } Second
   *[other] { $count } Seconds
}

## The timeline

# Screen readers name the list of messages by this.
chat-timeline = Messages
# A date divider. <b> marks the day, like "Today" or "Mon, 3 Nov"; $time is a time of day.
day-divider = <b>{ $day }</b>, { $time }
history-loading = Loading older messages…
history-failed = Couldn't load older messages
history-retry = Retry
# The divider above the first unread message. Shown in capitals.
chat-unread-divider = New
# The top of a channel's history. $name is the channel's name.
chat-start-channel-title = Welcome to #{ $name }
chat-start-channel = This is the beginning of the channel.
chat-start-channel-alone = This is the very beginning of the channel, say hello! 👋
chat-start-limited = This is the beginning of your history.
chat-start-notes = This is the beginning of your notes.
chat-start-dm = This is the beginning of your conversation.
unread-jump-lots = Lots of unread history: keep loading?
# $count is how many new messages, at most 99.
unread-jump-count = { NUMBER($count) }+ new messages

## A message request, above the composer

request-title = Message request
request-load-pictures = Accept to load media & profile
request-load = Accept to load media
request-block = Block
request-accept = Accept
# Reasons a custom emoji didn't load, shown as $why above.
emoji-why-cancelled = Cancelled
emoji-why-not-found = Not found
emoji-why-invalid = Invalid image
