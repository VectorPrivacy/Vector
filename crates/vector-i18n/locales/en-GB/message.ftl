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
