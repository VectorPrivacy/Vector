### The file preview before sending: compression, metadata, and a folder sent as a .zip.

## Compressing

# $percent is from 0 to 100.
preview-compressing-percent = Compressing... { $percent }%
preview-compressing = Compressing...
# $size is the smaller size, like "1.2 MB"; $percent is how much smaller.
preview-compress-saving = ~{ $size } ({ $percent }% smaller)
preview-compress-no-savings = No significant savings
preview-compress-failed = Couldn't compress this file
preview-compress-image = Compress Image
preview-compress-video = Compress Video
preview-keep-metadata = Keep Metadata
preview-keep-metadata-hint = Includes location, camera & date

## A folder, zipped

# $size is the .zip's size, like "4.5 MB"; $files is how many files it holds.
preview-zip-files = { $size } ({ $files ->
    [one] { $files } file
   *[other] { $files } files
})
# $size is the .zip's size; $files and $folders are how many of each it holds.
preview-zip-files-folders = { $size } ({ $files ->
    [one] { $files } file
   *[other] { $files } files
}, { $folders ->
    [one] { $folders } folder
   *[other] { $folders } folders
})
# $size is the .zip's size, which no media server will take.
preview-zip-too-large = { $size } — Too Large
# Below a folder's list when it holds more than is shown. $count is how many more.
preview-zip-more = ...and { $count } more
# In place of folders nested too deep to show.
preview-zip-deep = Deeply nested...

## Spoilers and buttons

preview-spoiler = Spoiler
preview-spoiler-remove = Remove spoiler
preview-spoiler-mark = Mark as spoiler
# Publishes a Mini App to the App Store.
preview-publish = Publish
preview-send = Send
# Keeps a staged attachment's edits and goes back to the composer.
preview-done = Done
# Takes a staged attachment out of the message.
preview-remove = Remove
preview-too-large = Too Large
# The file is still being prepared; it sends once it's ready.
preview-sending-when-ready = Sending when ready

## A folder that couldn't be zipped

# The .zip's name when several files go together.
preview-zip-name = Archive
# The .zip's name when the folder has no name of its own.
preview-zip-folder = folder
preview-zip-empty = Directory is empty
preview-zip-nothing = Nothing to zip
preview-zip-too-deep = Directory nesting too deep (>128 levels)
# $path is the folder; $error is why.
preview-zip-read-dir-failed = Failed to read directory { $path }: { $error }
# $error is why.
preview-zip-read-entry-failed = Failed to read entry: { $error }
# $error is why.
preview-zip-create-failed = Failed to create zip file: { $error }
# $error is why.
preview-zip-add-dir-failed = Failed to add directory: { $error }
# $error is why.
preview-zip-start-file-failed = Failed to start file in zip: { $error }
# $path is the file; $error is why.
preview-zip-read-file-failed = Failed to read file { $path }: { $error }
# $error is why.
preview-zip-write-failed = Failed to write to zip: { $error }
# $error is why.
preview-zip-finish-failed = Failed to finalize zip: { $error }
