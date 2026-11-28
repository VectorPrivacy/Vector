### A file in a message: its box, its download, and a Mini App's live session.

## Sending

file-not-sent = Not sent
file-sending = Sending…
# The badge over a media file the sender marked as a spoiler.
file-spoiler = Spoiler
file-accept-to-view = Accept to view

## Downloading

file-unavailable = File Unavailable
file-download-failed = Download Failed
file-resume-download = Resume Download
file-download = Download
# $type is the file's extension in capitals, like "PDF".
file-download-type = Download { $type }
file-gone = Deleted, or lost by its server
file-tap-retry = Tap to Retry
# $progress is how much has arrived, like "45%" or "2.1 MB".
file-resume-note = Resume download · { $progress }
# $progress is how much has arrived, like "45%" or "2.1 MB".
file-resume-done = { $progress } done
file-unknown-size = Unknown Size
# $reason is why, as the server or network gave it.
file-failed-retry = Failed: { $reason } · Tap to Retry
# $reason is why, as the server or network gave it.
file-failed = Failed: { $reason }

## Why a download failed, after "Failed:"

# $code is the server's HTTP status, like 404.
file-why-missing = Not on the server ({ $code })
# $code is the server's HTTP status, like 403.
file-why-refused = The server refused it ({ $code })
file-why-busy = The server is busy, try later
# $code is the server's HTTP status, like 503.
file-why-down = The server is down ({ $code })
file-why-timeout = The server took too long
file-why-unreachable = Couldn't reach the server
# $host is the website the Mini App comes from.
file-tap-to-load = Tap to Load · { $host }
file-unavailable-gone = Unavailable · Deleted, or lost by its server
file-download-failed-retry = Download Failed · Tap to Retry
# $kind is what the file is, like "PDF Document"; $size is its size, like "2.1 MB".
file-kind-size = { $kind } — { $size }

## A Mini App someone shared, and who is in it

file-app-playing = Playing
file-app-join = Click to Join
file-app-play = Click to Play
# $count is how many people are in it.
file-app-online = { $count ->
    [one] { $count } online
   *[other] { $count } online
}

## Why a download stopped

file-too-large-to-hold = Too large to hold without keeping it
file-already-downloading = This file is already downloading
file-memory-full = Too much media is open to show this one

## Saving a file out of Vector

file-not-on-device = This file isn't on this device any more
# $error is why.
file-save-failed = Couldn't save the file: { $error }
file-save-mismatch = Couldn't save the file: it didn't read back as sent

## Sending a file

file-empty = File is empty (0 bytes)
# $error is why.
file-metadata-failed = Failed to read file metadata: { $error }
# $error is why.
file-read-failed = Couldn't read the file: { $error }
