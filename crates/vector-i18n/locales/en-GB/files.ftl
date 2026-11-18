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
