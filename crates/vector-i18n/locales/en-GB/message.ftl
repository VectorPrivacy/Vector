### A message's right-click menu, and what its rows ask before they act.

## Menu rows

msg-retry = Retry send
msg-delete = Delete
# Delete, for a message whose signing key isn't on this device: it can't be fully removed.
msg-delete-limited = Delete (limited)
# A community moderator's delete: the message is hidden for everyone.
msg-hide = Hide
msg-cancel-upload = Cancel upload
msg-react = React
msg-reply = Reply
msg-pin = Pin
msg-unpin = Unpin
msg-edit = Edit
# A message with both a file and text says which of the two each copy row takes;
# otherwise the row is just msg-copy.
msg-copy = Copy
msg-copy-file = Copy File
msg-copy-text = Copy Text
# Dimmed beside a second copy row that keeps the message's formatting marks.
msg-copy-markdown = (markdown)
# Saves a file that lives only in the app's encrypted store.
msg-save = Save
msg-reveal = Reveal in folder
msg-mark-unread = Mark as Unread
msg-copy-link = Copy Link

## Toasts

msg-link-copied = Copied Message Link
msg-file-copied = Copied file to clipboard

## Confirmations. <br> is a line break and <b> is bold.

msg-delete-title = Delete this message?
msg-delete-text = Are you sure you want to delete this message?
msg-delete-limited-title = Limited Delete
# The message has a file attached.
msg-delete-limited-file = This message can't be fully removed from relays because the signing key isn't on this device (sent from another device, or predates deletion support).<br><br>We can still:<br><b>Delete the attached file</b> from the storage server.<br><b>Notify other Vector users</b> to drop the message.<br><br>Continue?
msg-delete-limited-text = This message can't be fully removed from relays because the signing key isn't on this device (sent from another device, or predates deletion support).<br><br>We can still <b>notify other Vector users</b> to drop the message from their copy.<br><br>Continue?
msg-hide-title = Hide this message?
msg-hide-text = Permanently hide this message for everyone in the community. This can't be undone.
msg-hide-failed = Hide Failed
msg-react-failed = Reaction Failed
msg-delete-failed = Delete Failed
msg-edit-failed = Edit Failed
msg-retry-impossible = Cannot retry
msg-react-denied = You can't react in this channel
# $error is why, as the relay or app gave it.
msg-react-not-sent = Reaction not sent: { $error }
# $error is why, as the relay or app gave it.
msg-not-sent = Not sent: { $error }
msg-accept-to-download = Accept the request to download files
msg-save-failed = Save Failed
msg-file-gone = This file isn't on this device any more
# Jumping to a message, by link or from Pins, that can't be reached.
msg-jump-unshown = That message can't be shown
msg-jump-not-local = That message isn't in your local history

## The toolbar over a hovered message: each is its button's tooltip, alongside msg-retry,
## msg-cancel-upload, msg-reply, msg-edit, msg-copy, msg-save and msg-reveal above

# $why is why the send failed.
msg-bar-retry-why = Retry send. { $why }
msg-bar-delete-failed = Delete failed message
msg-bar-react = Add reaction
msg-bar-delete = Delete message
# The signing key isn't on this device, so only part of it can be deleted.
msg-bar-delete-limited = Delete message (limited)
msg-bar-hide = Hide message

## The timer on a message that will disappear

# $left is a countdown, like "2h 5m".
msg-dissolves-in = Message dissolves in { $left }
msg-dissolving = Dissolving...

## A community invite link, shown as a card

# Shown in capitals.
invite-card-title = Community Invite
invite-card-unavailable = Invite Unavailable
invite-card-unavailable-text = This invite could not be loaded — it may be revoked or expired.
# Opens a community you're already in.
invite-card-open = Open
invite-card-join = Join
