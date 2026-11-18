## A media server's dialog: its state, this device's speeds to it, the plan it puts this
## account on, and what uploads have taught Vector it accepts.

server-notice-offline = Vector couldn’t reach this server recently. Uploads are routed elsewhere until it answers again.
server-notice-maxed = An allowance on this server is used up. Uploads are routed elsewhere until it frees up.
server-notice-no-access = This server won’t take uploads from your account.
server-notice-disabled = Disabled — uploads aren’t routed here.
server-remove = Remove
server-add-failed = Could not add server
server-disable = Disable
server-enable = Enable
server-latency = Latency
server-upload = Upload
server-best = Best
server-unit-ms = ms
server-unit-mbits = Mbit/s
# $speed is a number of megabits a second.
server-mbits = { $speed } Mbit/s
# $count is how many files this device has uploaded there.
server-uploads = { $count ->
    [one] { $count } upload
   *[other] { $count } uploads
}
# $count is how many of the latest uploads the chart shows.
server-speed-of-last = { $count ->
    [one] Speed of your last upload
   *[other] Speed of your last { $count } uploads
}
# A meter's limit. $size is an amount of data, like "4.2 GB".
server-meter-of = of { $size }
# $size is a file size, like "100 MB".
server-accepts-up-to = Accepts files up to { $size }.
server-no-access = No access
server-member = Member
server-your-plan = Your plan
server-per-file = per file
server-storage = Storage
# $count is how many files are stored there.
server-files = { $count ->
    [one] { $count } file
   *[other] { $count } files
}
server-today = Today
server-resets = resets 00:00 UTC
server-caps-title = What we’ve learned
server-caps-blurb = This server doesn’t say what it accepts, so Vector learns as you send: the largest file it has taken of each type, and the types it has refused. Uploads are routed to the best-suited server automatically.
server-caps-loading = Loading…
server-caps-error = Could not load capability data.
server-caps-none = No capability data yet. Vector learns each server’s file-type and size limits as you send files.
server-caps-accepts = Accepts
server-caps-limited = Size-limited
server-caps-rejects = Rejects
server-cap-encrypted = Encrypted
    .tip = Tested with encrypted chat data
server-cap-public = Public
    .tip = Tested with public uploads (avatar, banner, etc.)
# $size is a file size, like "25 MB".
server-cap-max = { $size } max
# $size is a file size, like "25 MB".
server-cap-rejects-from = rejects ≥ { $size }

## Why a media server couldn't be added or changed

server-exists = Server already exists
server-not-found = Server not found
server-is-default = Cannot add a default server as custom
