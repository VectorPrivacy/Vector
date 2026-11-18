### Emoji packs: the pack menu, and making or editing a pack.

## A pack's menu in the emoji picker

packs-share = Share Pack
packs-edit = Edit Pack
packs-remove = Remove Pack

## An emoji's menu while making a pack

packs-emoji-rename = Rename Emoji
packs-emoji-delete = Delete Emoji

## Adding and removing packs

packs-remove-failed = Couldn't Remove Pack
packs-add-failed = Couldn't Add Pack
packs-equipped = Pack equipped
packs-full-title = Pack Slots Full!
# $max is how many packs can be equipped at once.
packs-full-text = Vector supports up to { $max } equipped packs. Remove one to add another.
packs-full-hint = Remove a Pack First.

## Making or editing a pack. An error shows a small line above its title, then a title,
## then the detail. Buttons are shown in capitals.

packs-name-placeholder = Pack name
packs-save = Save Pack
packs-upload = Upload Emoji
# $max is how many emojis a pack can hold.
packs-upload-full = Maximum { $max } reached
packs-icon-gone = This pack icon is no longer available (it may have been deleted).
packs-emoji-gone = This emoji is no longer available (it may have been deleted).
packs-got-it = Got It
packs-try-again = Try Again
packs-error-title = Please Try Again.
packs-skip = Skip
packs-discard = Discard
packs-delete = Delete

## Naming an emoji

packs-name-title = Name This Emoji
packs-shortcode-rules = Letters, numbers, and underscores only.
# Which emoji is being named, shown in capitals. $done is this one and $total how many there are.
packs-name-progress = { $done } of { $total }
# $shortcode is the name, written between colons like :party:.
packs-shortcode-used = :{ $shortcode }: is already used in this pack.
packs-shortcode-taken = Oops! Name Already Taken!
# $shortcode is the name, written between colons like :party:.
packs-shortcode-taken-text = :{ $shortcode }: is already used by another emoji in this pack. Pick a different name.
packs-shortcode-taken-title = Duplicate Shortcode.

## Adding images

packs-crop-failed = Oops! Couldn’t Crop That!
packs-too-big = Oops! File Size Exceeded!
packs-too-big-text = File Size must be under 256Kb.
packs-crop-too-big = Oops! Too Big After Cropping!
packs-crop-too-big-text = Cropping this to a square re-encoded it over the 256 KB limit. Try an already-square version, or shrink it a little before adding.
packs-crop-too-big-title = Crop Pushed It Over.
packs-bad-format = Oops! Unsupported Format!
packs-bad-format-text = Use PNG, JPG, GIF, or WebP.
packs-unreadable = Oops! Couldn’t Read That!
# $count is how many images couldn't be imported.
packs-unreadable-text = { $count ->
    [one] That image couldn’t be imported. Try PNG, JPG, GIF, or WebP.
   *[other] { $count } images couldn’t be imported. Try PNG, JPG, GIF, or WebP.
}
packs-unreadable-title = Unsupported Image.

## An emoji that can't be used any more

packs-emoji-large = Oops! Emoji Too Large!
packs-emoji-large-text = This emoji is over the 1 MB size limit, so it cannot be used. Compress it under 1 MB, then delete this one and upload the smaller version.
packs-emoji-large-title = Compress & Re-upload.
packs-emoji-unavailable = Oops! Emoji Unavailable!
packs-emoji-unavailable-text = This emoji has been deleted by its host. Re-uploading will fix it: delete this emoji, then upload it again.
packs-emoji-unavailable-title = Delete It & Re-upload.

## Saving

packs-need-emoji = Oops! Add an Emoji First!
packs-need-emoji-text = A pack needs at least one emoji. Add one to save it, or delete it to discard the draft.
packs-need-name = Oops! Name Your Pack First!
packs-need-name-text = Give it a name to save it, or delete it to discard the draft.
packs-need-shortcodes = Oops! Your Emojis Need Names!
packs-need-shortcodes-text = Give each emoji a name, then save.
packs-save-failed = Couldn’t Save Pack
# The error itself follows on its own line.
packs-save-failed-text = Your emojis are safe, nothing was lost. An upload didn’t finish (usually a passing network glitch). Tap Done to retry.
packs-save-failed-title = Your Progress Is Safe.

## Discarding or deleting a pack

packs-discard-title = Discard This Pack?
packs-discard-text = This pack was never saved. The emojis you added will be lost.
packs-delete-title = Delete This Pack?
packs-delete-text = This action permanently removes the emoji files from your media servers and the pack from Nostr. This action cannot be undone.
packs-deleting = Deleting Pack
# $count is how many files.
packs-deleting-files = { $count ->
    [one] Removing { $count } file from media servers…
   *[other] Removing { $count } files from media servers…
}
packs-deleting-nostr = Removing from Nostr…
# $done is how many emojis are deleted so far and $total how many there are.
packs-deleting-emoji = Deleting emoji { $done } of { $total } from media servers…
# $done is how many files are deleted so far and $total how many there are.
packs-deleting-icon = Deleting pack icon ({ $done } of { $total })…
# $count is how many files.
packs-deleting-orphans = { $count ->
    [one] Cleaning up { $count } orphan file…
   *[other] Cleaning up { $count } orphan files…
}
packs-deleting-pack = Removing pack from Nostr…

## A pack's card and details

packs-loading = Loading pack…
packs-unavailable = Pack unavailable
packs-fetch-failed = Failed to fetch
# A pack with no name.
packs-untitled = Untitled
packs-empty = Empty pack
packs-removing = Removing…
packs-adding = Adding…
packs-card-remove = Remove
packs-add-pack = Add Pack
packs-copy-share-link = Copy share link
# $count is how many emoji the pack holds.
packs-emoji-count = { $count ->
    [one] { $count } emoji
   *[other] { $count } emojis
}

## Why an emoji or pack couldn't be uploaded

packs-image-unreadable = Image couldn't be read (unsupported or corrupt file)
# $size is the file's size and $max the limit, both in KB.
packs-file-too-large = File is { $size } KB, max is { $max } KB.
packs-file-empty = File is empty.
packs-no-media-servers = No Blossom servers configured.
packs-needs-emoji = A pack needs at least one emoji.
