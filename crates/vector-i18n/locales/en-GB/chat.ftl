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
