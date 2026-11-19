### Text Vector's engine writes for people: errors, network status, moderation policies.
### The app matches each line it receives to one of these, so a message here appears
### wherever the engine's English used to.

## Uploading a file to a media server.

# $seconds: a number of seconds
# $sent: a number of bytes
# $server: a server's address
# $total: a number, the whole amount
core-upload-stalled-accepted-nothing-bytes = Upload stalled: { $server } accepted nothing for { $seconds }s ({ $sent } of { $total } bytes sent)
# $seconds: a number of seconds
# $server: a server's address
# $total: a number, the whole amount
core-upload-stalled-received-all-bytes = Upload stalled: { $server } received all { $total } bytes but gave no answer in { $seconds }s
# $error: a technical reason, often in English
core-upload-request-failed = Upload request failed: { $error }
core-upload-no-upload-attempts-were = No upload attempts were made
# $server: a server's address
core-upload-accepted-upload-but-blob = { $server } accepted the upload but the blob is not retrievable
core-upload-no-media-server-configured = No media server is configured.
# $host: a server's address
# $kind: a file or event type
core-upload-accept-files = { $host } doesn't accept { $kind } files.
# $host: a server's address
# $size: a size, such as 2 MB
core-upload-refused-files-before = { $host } has refused files of { $size } before.
# $host: a server's address
# $limit: the limit, such as a count or a size
# $used: how much is used, such as a size
core-upload-today-upload-allowance-used = Today's { $limit } upload allowance on { $host } is used up ({ $used } uploaded so far). It resets at midnight UTC.
# $host: a server's address
core-upload-today-s-upload-allowance-on-is = Today's upload allowance on { $host } is used up. It resets at midnight UTC.
# $host: a server's address
# $limit: the limit, such as a count or a size
# $used: how much is used, such as a size
core-upload-storage-full-delete-some = Your storage on { $host } is full ({ $used } of { $limit }). Delete some files there, or use another server.
# $host: a server's address
core-upload-storage-full = Your storage on { $host } is full.
# $host: a server's address
# $limit: the limit, such as a count or a size
core-upload-allows-files-up-account = { $host } allows files up to { $limit } for your account.
# $host: a server's address
core-upload-accept-file-large = { $host } won't accept a file this large.
# $host: a server's address
core-upload-accept-kind-file = { $host } doesn't accept this kind of file.
# $detail: more detail, often in English
# $host: a server's address
core-upload-only-takes-uploads-from = { $host } only takes uploads from Vector accounts it recognises. { $detail }
# $host: a server's address
# $seconds: a number of seconds
core-upload-rate-limiting-try-again = { $host } is rate-limiting you; try again in { $seconds }s.
# $host: a server's address
core-upload-is-rate-limiting-you-try-again = { $host } is rate-limiting you; try again shortly.
# $host: a server's address
core-upload-out-space = { $host } is out of space.
# $host: a server's address
core-upload-blocked-file = { $host } has blocked this file.
core-upload-vector-checking-time-relays = Vector is checking the time with your relays; try again in a moment.
core-upload-turn-decentralised-clock-settings = Turn on Decentralised Clock in Settings, or set this device's clock to update automatically.
# $host: a server's address
core-upload-rejected-upload-signature-timestamp = { $host } rejected the upload signature's timestamp — this device's clock may be wrong.
# $detail: more detail, often in English
# $host: a server's address
core-upload-rejected-upload-authorization = { $host } rejected the upload authorization ({ $detail }).
# $host: a server's address
core-upload-hit-internal-error = { $host } hit an internal error.
# $detail: more detail, often in English
# $host: a server's address
core-upload-refused-vector-upload-signature = { $host } refused Vector's upload signature ({ $detail }).
# $host: a server's address
# $status: an HTTP status code
core-upload-having-trouble-http = { $host } is having trouble (HTTP { $status }).
core-upload-request-failed-short = Upload request failed
core-upload-cancelled = Upload cancelled
core-upload-stalled = Upload stalled
# $host: a server's address
core-upload-stopped-accepting-file-mid = { $host } stopped accepting the file mid-way.
# $host: a server's address
core-upload-could-not-reached = { $host } could not be reached.
# $host: a server's address
core-upload-altered-file-instead-storing = { $host } altered the file instead of storing it.
core-upload-no-media-server-is-configured-add = No media server is configured. Add one in Settings → Network.
core-upload-no-media-server-accepted = No media server accepted the file:
core-upload-i2p-only-media-servers = I2P-Only is on, so your media servers are off.
# $host: a server's address
core-upload-take-uploads-from-account = { $host } won't take uploads from this account.
# $bytes: a size, such as 2 MB
# $host: a server's address
core-upload-allows-files-up = { $host } allows files up to { $bytes }.
# $error: a technical reason, often in English
core-upload-invalid-server-url = Invalid server URL: { $error }
# $host: a server's address
# $span: a length of time, such as 5 minutes
# $hint: what to do about it, a sentence of its own
core-upload-clock-slow = { $host } says this device's clock is about { $span } slow. { $hint }
# $host: a server's address
# $span: a length of time, such as 5 minutes
# $hint: what to do about it, a sentence of its own
core-upload-clock-fast = { $host } says this device's clock is about { $span } fast. { $hint }
# A length of time, placed in a longer line.
core-span-seconds = { $count ->
    [one] { $count } second
   *[other] { $count } seconds
}
# A length of time, placed in a longer line.
core-span-minutes = { $count ->
    [one] { $count } minute
   *[other] { $count } minutes
}
# A length of time, placed in a longer line.
core-span-hours = { $count ->
    [one] { $count } hour
   *[other] { $count } hours
}
# A length of time, placed in a longer line.
core-span-days = { $count ->
    [one] { $count } day
   *[other] { $count } days
}

## Adding a media server.

core-media-server-url-cannot-empty = URL cannot be empty
core-media-server-url-must-use-https = URL must use https:// or http://
core-media-server-url-must-include-host = URL must include a host
# $error: a technical reason, often in English
core-media-server-invalid-url = Invalid URL: { $error }

## Reacting to a message.

core-reaction-message-reached-reaction-limit = This message has reached its reaction limit
core-reaction-used-all-new-reactions = You've used all your new reactions on this message

## One-to-one calls.

core-call-calls-not-available = Calls are not available
core-call-voice-available-platform-yet = Voice isn't available on this platform yet
core-call-not-available-platform = Not available on this platform
core-call-not-valid-npub = Not a valid npub
core-call-leave-voice-channel-first = Leave the voice channel first
core-call-already-call = Already in a call
core-call-no-incoming-call-accept = No incoming call to accept
core-call-no-call = No call
core-call-they-cannot-receive-video = They cannot receive video
core-call-screen-recording-off-vector = Screen recording is off for Vector in System Settings

## Voice channels in a community.

core-voice-not-call = Not in this call
# $error: a technical reason, often in English
core-voice-unavailable = Voice unavailable: { $error }
core-voice-end-call-first = End your call first
core-voice-channel = That isn't a voice channel
core-voice-channel-key-hasn-arrived = This channel's key hasn't arrived yet
core-voice-connect-channel = You can't connect to this channel
core-voice-share-video-channel = You can't share video in this channel
core-voice-not-in-that-call = Not in that call
core-voice-moderator-muted-call = A moderator muted you in this call
core-voice-moderator-deafened-call = A moderator deafened you in this call
core-voice-video-ready-call-yet = Video isn't ready in this call yet
core-voice-they-share-video-here = They can't share video here
core-voice-no-such-channel = No such channel

## Moderation policies a community can start from: names, descriptions, examples and settings.

# $count: a number
core-preset-one-message = { $count ->
    [one] { $count } hit
   *[other] { $count } hits
} in one message
# $count: a number
core-preset-window = { $count ->
    [one] { $count } hit
   *[other] { $count } hits
} in the window
# $thresholds: when the rule trips, a list of its own
core-preset-trips = Trips at { $thresholds }.
# $count: a number
core-preset-list-matched-whole-words = { $count ->
    [one] { $count } word
   *[other] { $count } words
} on the list, matched as whole words unless you wrap them in *.
core-preset-link-domains = Link domains
# $count: a number
core-preset-blocked-subdomains-included = { $count ->
    [one] { $count } domain
   *[other] { $count } domains
} blocked, subdomains included.
core-preset-same-message-repeated = Same message repeated
# $minutes: a number of minutes
core-preset-one-account-posting-same = One account posting the same thing over and over inside { $minutes ->
    [one] { $minutes } minute
   *[other] { $minutes } minutes
}. Case, punctuation and digits are ignored, so changing them does not help.
core-preset-one-account-posting-the-same-thing = One account posting the same thing over and over. Case, punctuation and digits are ignored, so changing them does not help.
core-preset-posting-too-fast = Posting too fast
# $seconds: a number of seconds
core-preset-one-account-messages-counted = One account's messages counted over any { $seconds ->
    [one] { $seconds } second
   *[other] { $seconds } seconds
}, whatever they say.
core-preset-mass-tagging = Mass tagging
core-preset-how-many-distinct-people = How many DISTINCT people one message names. Tagging the same person repeatedly counts once.
core-preset-many-accounts-one-line = Many accounts, one line
# $count: a number
core-preset-more-separate-accounts-posting = { $count } or more separate accounts posting the same line at the same time. The shape of a raid.
core-preset-join-flood = Join flood
# $count: a number
# $minutes: a number of minutes
core-preset-more-joins-within-minutes = { $count } or more joins within { $minutes ->
    [one] { $minutes } minute
   *[other] { $minutes } minutes
}.
core-preset-new-account = New account
# $hours: a number of hours
core-preset-joined-less-than-hours = Joined less than { $hours ->
    [one] { $hours } hour
   *[other] { $hours } hours
} ago.
core-preset-barely-posted = Barely posted
# $count: a number
core-preset-posted-most = Has posted at most { $count ->
    [one] { $count } message
   *[other] { $count } messages
}.
core-preset-someone-posts-word-from = Someone posts a word from your list.
core-preset-words-catch = Words to catch
core-preset-one-per-line-use = One per line. Use *word* to match inside longer words.
core-preset-someone-posts-link-domain = Someone posts a link to a domain on your list.
core-preset-domains-block = Domains to block
core-preset-one-per-line-matches = One per line. Matches the domain and its subdomains.
core-preset-one-account-posting-same-thing-over-over = One account posting the same thing over and over inside half an hour. Case, punctuation and digits are ignored, so changing them does not help.
core-preset-one-message-naming-crowd = One message naming a crowd. Counts distinct people, so twenty pings at one person is one person — annoying, but not a raid.
core-preset-one-account-posting-faster = One account posting faster than a person reasonably types, whatever they are saying. Catches a spammer who varies their text just enough to look different every time.
core-preset-over-how-many-seconds = Over how many seconds
core-preset-span-counts-within-10 = The span it counts within. 10 catches a burst; 60 catches a sustained flood.
core-preset-separate-accounts-posting-same = Separate accounts posting the same line at the same time. The shape of a raid.
core-preset-vector-defaults = Vector's Defaults
core-preset-raid-detection-running-here = Raid detection, running here already. Open it to read every rule, or save your own version.
core-preset-swarm-fresh-accounts-posting = A swarm of fresh accounts posting one line
core-preset-built-anti-raid-policy = A Built-In Anti-Raid Policy: detects raids proactively and alerts you with a list of suspects for quick handling.
core-preset-what-these-rules-do = What these rules do
core-preset-every-rule-runs-here = Every rule that runs here, in order.
core-preset-how-much-takes-trip = How much it takes to trip a rule, and how confident the result is.
core-preset-scam-links = Scam Links
core-preset-blocks-link-shorteners-redirectors = Blocks link shorteners and redirectors that scams hide behind.
core-preset-claim-airdrop-bit-ly = Claim your airdrop at bit.ly/…
core-preset-only-catches-shorteners-bundled = Only catches the shorteners on the bundled list, plus any domain you add.
core-preset-also-block-these-domains = Also block these domains
core-preset-one-per-line-bundled = One per line. The bundled shortener list is always included.
core-preset-never-flag-these-domains = Never flag these domains
core-preset-own-links-docs-anything = Your own links, docs, anything you trust.
core-preset-no-spam = No Spam
core-preset-same-message-over-over = The same message over and over, from one account or many.
core-preset-same-pitch-twelve-times = The same pitch, twelve times
core-preset-counts-shapes-not-words = Counts shapes, not words: changing case, punctuation or digits does not help a spammer.
core-preset-word-filter = Word Filter
core-preset-any-list-words-choose = Any list of words you choose, with per-channel exceptions.
core-preset-spoilers-slurs-scam-phrase = Spoilers, slurs, or a scam phrase
core-preset-whole-words-default-filtering = Whole words by default, so filtering "art" leaves "start" alone.
core-preset-allowed-these-channels = Allowed in these channels
core-preset-filter-ignores-these-channels = The filter ignores these channels entirely.
core-preset-mass-tagging-name = Mass Tagging
core-preset-one-message-names-crowd = One message that names a crowd, rather than a person.
core-preset-post-tagging-twelve-people = A post tagging twelve people at once
core-preset-counts-distinct-people-per = Counts distinct people per message: pinging one person twenty times reads as one, because it is.
core-preset-announcement-channels-where-tagging = Announcement channels where tagging everyone is the point.
core-preset-rate-limit = Rate Limit
core-preset-too-many-messages-too = Too many messages too quickly from one account, whatever they say.
core-preset-twelve-messages-ten-seconds = Twelve messages in ten seconds
core-preset-counts-messages-not-content = Counts messages, not content: a fast typer in a lively channel can trip it, so preview before enabling.
core-preset-start-from-scratch = Start from scratch
core-preset-empty-policy-name-add = An empty policy. Name it, add the rules you want, preview before anything runs.
core-preset-whatever-community-actually-needs = Whatever this community actually needs
core-preset-policy-no-rules-catches = A policy with no rules catches nothing, and the preview will tell you so.
core-preset-policy-name = Policy name
core-preset-what-will-see-list = What you will see in the list.
core-preset-add-many-like-any = Add as many as you like. Any one of them can convict on its own.
core-preset-these-channels-ignored-entirely = These channels are ignored entirely.
core-preset-new-policy = New policy
core-preset-words = Words
core-preset-patterns = Patterns
core-preset-rules = Rules
# How a pattern rule matches: regular expressions, counted.
core-preset-expressions = { $count ->
    [one] { $count } expression
   *[other] { $count } expressions
} matched against each message.
# A threshold on its own, in a list after "Trips at".
core-preset-hits = { $count ->
    [one] { $count } hit
   *[other] { $count } hits
}
# Thresholds in order: "3 hits in one message, then 5 hits in the window".
# $first: the thresholds so far
# $next: the next threshold
core-preset-then = { $first }, then { $next }

## Why a moderation policy flagged a member, in its preview.

# $count: a number
# $quoted: the message, quoted
core-policy-posted-same-message-other = Posted the same message as { $count } other { $count ->
    [one] member
   *[other] members
} — “{ $quoted }”
# $count: a number
core-policy-joined-burst = Joined in a burst of { $count }
core-policy-barely-posted = Has barely posted
# $count: a number
core-policy-repeated-one-message-times = Repeated one message { $count ->
    [one] once
   *[other] { $count } times
}
# $count: a number
core-policy-posted-flagged-link = Posted { $count ->
    [one] { $count } flagged link
   *[other] { $count } flagged links
}
# $count: a number
# $domains: web addresses, separated by commas
core-policy-posted-flagged-link-s = Posted { $count ->
    [one] { $count } flagged link
   *[other] { $count } flagged links
}: { $domains }
# $count: a number
# $name: a name
core-policy-used-time = Used { $name } { $count ->
    [one] (once)
   *[other] ({ $count } times)
}
# $rule: the rule's name
core-policy-matched-rule = Matched rule { $rule }
core-policy-long-standing-member = Long-standing member
# $count: a number
# $seconds: a number of seconds
core-policy-sent-messages = Sent { $count ->
    [one] { $count } message
   *[other] { $count } messages
} in { $seconds }s

## Why the raid check suspects a member, or clears one.

core-raid-community-owner = Community owner
core-raid-holds-role = Holds a role
# $count: a number
core-raid-posted-same-message-other = Posted the same message as { $count } other { $count ->
    [one] member
   *[other] members
}
core-raid-joined-last-24h = Joined in the last 24h
core-raid-never-posted = Has never posted
# $count: a number
core-raid-only-message = Only { $count ->
    [one] { $count } message
   *[other] { $count } messages
}
core-raid-repeats-one-message = Repeats one message
core-raid-long-standing-active-member = Long-standing, active member
core-raid-varied-conversation-history = Varied conversation history

## Communities: permissions, roles, channels, invites and moderation. Errors start with a small letter in English because the app shows them as they are written; begin them however suits your language.

core-community-svg-supported-community-icons = SVG isn't supported for community icons or banners
core-community-relay-stopped-answering = The relay stopped answering
# $network: a network: Tor or I2P
core-community-still-connecting-wait-moment = { $network } is still connecting. Wait a moment and try again.
# $network: a network: Tor or I2P
core-community-none-community-relays-reachable = None of this community's relays are reachable on { $network }.
core-community-no-relay-accepted-event = No relay accepted the event
# $seconds: a number of seconds
core-community-no-relay-accepted-the-event-within = No relay accepted the event within { $seconds }s
core-community-only-community-staff-hold = Only community staff hold this community's write key; ask an admin to re-send your promotion
# $label: an invite link's label
core-community-already-invite-link-labeled = You already have an invite link labeled “{ $label }”. Pick a different label.
# $action: what was being done, in English
# $permission: a permission's name
core-community-needs-permission = { $action } needs the { $permission } permission
core-community-category-needs-name = A category needs a name
# $max: a number, the most allowed
core-community-holds-most-categories = A community holds at most { $max } categories
core-community-no-room-another-category = No room for another category
core-community-no-room-between-those = No room between those categories
core-community-no-room-between-those-channels = No room between those channels
core-community-category-sets-own-permissions = This category sets its own permissions, so a channel in it takes Manage Roles over them
core-community-duplicate-private-channel-read = You can't duplicate a private channel you can't read
core-community-channel-permissions-set-above = This channel has permissions set above your roles, so only someone above you can duplicate it
# $error: a technical reason, often in English
core-community-channel-made-but-not = The channel was made, but not everything was copied onto it: { $error }
core-community-no-room-order-these = No room to order these
core-community-making-channel-private-needs = Making a channel private needs the community on the new roles
core-community-only-edit-private-channel = You can only edit a private channel you can read
core-community-channel-public-every-member = Channel is public: every member already reads it
core-community-channel-is-public-every-member-already = Channel is public — every member already reads it
core-community-not-authorized-manage-channel = Not authorized to manage this channel's access
core-community-channel-no-permission-less = Channel has no permission-less access role to grant
core-community-channel-public-there-no = Channel is public — there is no access to revoke
core-community-owner-supreme-cannot-removed = The owner is supreme and cannot be removed
core-community-channel-access-role-not = This channel's access role has not folded yet — retry once the control plane serves it
core-community-we-hold-no-key = We hold no key for this channel, so we cannot rotate it
core-community-roster-read-whole-no = The roster couldn't be read whole, so no channel was rotated yet
core-community-the-roster-couldn-t-be-read = The roster couldn't be read whole, so no channel was rotated yet; retrying on the next follow
core-community-too-many-invite-links = Too many invite links to sync: revoke some, or let them expire, first
core-community-private-channel-key-not = This private channel's key has not arrived yet
# $error: a technical reason, often in English
core-community-copy-invite-list-read = A copy of your invite list couldn't be read, so it isn't written over: { $error }
core-community-channel-pins-sealed-under = This channel's pins are sealed under a key you don't hold; pinning would erase them
# $max: a number, the most allowed
core-community-channel-already-holds-pins = This channel already holds { $max } pins; unpin one first
core-community-message-not-found-device = Message not found in this device's history
core-community-message-wrap-no-longer = The message's wrap is no longer served by this community's relays
core-community-message-from-key-epoch = That message is from a key epoch this device no longer holds
core-community-message-seal-form-cannot = This message's seal form cannot be pinned
core-community-message-proof-did-not = This message's proof did not verify
core-community-message-from-root-epoch = That message is from a root epoch this device no longer holds
core-community-no-invite-list-found = No invite list found to revoke from
core-community-control-plane-read-whole = The Control Plane couldn't be read whole
core-community-invite-expired = This invite has expired
core-community-invite-link-been-revoked = This invite link has been revoked
core-community-invite-bundle-not-found = Invite bundle not found on relays
# $time: a date and time
core-community-timed-out-here-until = You're timed out here until { $time }
core-community-not-authorized-kick-member = Not authorized to kick this member
core-community-only-time-out-members = You can only time out members beneath you, with Timeout Members
core-community-nickname-hold-line-breaks = A nickname can't hold line breaks or control characters
# $max: a number, the most allowed
core-community-nickname-most-characters = A nickname is at most { $max } characters
core-community-change-nickname-here = You can't change your nickname here
core-community-only-change-nicknames-members = You can only change the nicknames of members beneath you, with Manage Nicknames
core-community-do-not-outrank-member = You do not outrank this member
core-community-rescuing-stranded-members-requires = Rescuing stranded members requires rotation authority (owner or BAN)
core-community-rescue-requires-split-community = Rescue requires a split community (no control pk held)
core-community-dissolved-accepts-no-new = This community is dissolved — it accepts no new moderation actions
core-community-hide-message-channel = You can't hide that message in this channel
core-community-only-owner-dissolve-community = Only the owner can dissolve a community
core-community-been-dissolved-cannot-founded = This community has been dissolved; it cannot be re-founded
core-community-founding-requires-ban-permission = Re-founding requires the BAN permission and outranking every removed member
core-community-control-plane-too-deep = The community's control plane is too deep to read in full right now; re-founding stopped so no member is left behind.
core-community-do-member = You can't do that to this member
core-community-move-them-voice-channel = Move them to a voice channel
core-community-banned-from-community = You are banned from this community
core-community-fragment-would-not-open = A fragment would not open, and it may be the newest copy — refusing to write
core-community-no-relay-we-write = No relay we write to answered the read
# $count: a number
# $total: a number, the whole amount
core-community-hold-fragments-refusing-rewrite = Hold { $count } of { $total } fragments — refusing to rewrite a set we haven't fully read
core-community-tombstone-published-but-relays = The tombstone was published but isn't on the relays
core-community-admin-rights-synced-device = Your admin rights aren't synced on this device yet — reopen the community and retry
# $new: a number
# $old: a number
core-community-founded-mid-action-epoch = The community re-founded mid-action (epoch { $old } -> { $new }); retry
core-community-hold-community-staff-write = You don't hold this community's staff write key, so you can't promote to staff yet
core-community-held-staff-write-key = This community's held staff write key is corrupt; re-sync before promoting
core-community-only-community-owner-grant = Only the community owner can grant @admin
core-community-only-community-owner-revoke = Only the community owner can revoke @admin
core-community-managing-roles-needs-manage = Managing roles needs the Manage Roles permission
core-community-role-current-state-could = This role's current state could not be fetched; try again once relays serve the control plane
# $max: a number, the most allowed
core-community-holds-most-roles = A community holds at most { $max } roles
core-community-there-no-room-beneath = There's no room beneath your role for a new one; reorder the roles first
core-community-give-role-permissions-hold = You can't give a role permissions you don't hold
core-community-only-edit-roles-beneath = You can only edit roles beneath your own
core-community-role-channel-changed-once = A role's channel can't be changed once it's created
# $role: a role's name
core-community-move-permissions-hold = You can't move "{ $role }": it has permissions you don't hold
core-community-still-uses-legacy-roster = This community still uses the legacy roster
core-community-roles-read-try-again = The community's roles couldn't be read; try again
core-community-no-room-left-bottom = No room left at the bottom; reorder the roles first
core-community-only-move-roles-beneath = You can only move roles beneath your own, to a place beneath your own
core-community-no-room-between-those-roles-move = No room between those roles; move a neighbour first
# $role: a role's name
core-community-moving-there-would-take = Moving "{ $role }" there would take it from members you can't give it to
core-community-only-delete-roles-beneath = You can only delete roles beneath your own
core-community-give-everyone-permissions-hold = You can't give everyone permissions you don't hold
core-community-only-channel-permissions-set = Only channel permissions can be set per channel
core-community-no-such-channel-category = No such channel or category
core-community-owner-holds-every-permission = The owner holds every permission everywhere
core-community-only-set-permissions-hold = You can only set permissions you hold, for roles and members beneath you
core-community-syncing-swaps-permissions-set = Syncing swaps permissions you can't set; ask someone who can
core-community-change-who-see-private = You can't change who can see this private channel: you don't have access to it yourself.
core-community-device-hold-channel-key = This device doesn't hold that channel's key
core-community-active-signer-account = The active signer isn't this account
core-community-roster-read-whole-no-key-delivered = The roster couldn't be read whole; no key was delivered
core-community-only-owner-moves-community = Only the owner moves a community to the new roles
core-community-already-uses-new-roles = This community already uses the new roles
# $max: a number, the most allowed
core-community-moves-new-roles-most = A community moves to the new roles with at most { $max } channels
core-community-too-many-roles-carry = Too many roles to carry across
core-community-the-control-plane-couldn-t-be = The control plane couldn't be read whole; nothing was changed
core-community-member-holds-permissions-their = This member holds permissions you don't, so their decisions can't be kept; ask someone who holds them
core-community-member-decided-more-than = This member decided more than one record can keep; nothing was changed
core-community-member-earlier-record-being = This member's earlier record isn't being served; nothing was changed, try again shortly
core-community-only-owner-sets-owner = Only the owner sets the owner's roles
core-community-owner-holds-roles-only = The owner holds roles only on the new roles
core-community-only-change-roles-members = You can only change the roles of members beneath you
# $role: a role's name
core-community-grant-beneath-own-role = You can't grant "{ $role }": it isn't beneath your own role
# $role: a role's name
core-community-remove-beneath-own-role = You can't remove "{ $role }": it isn't beneath your own role
# $max: a number, the most allowed
core-community-member-holds-most-roles = A member holds at most { $max } roles
# $channels: how many private channels, in English
# $what: what was asked for, in English
core-community-give-opens-access-someone = You can't give { $what }: it opens { $channels } you don't have access to. Someone with access has to, or get access first.
core-community-member-current-grant-could = This member's current grant could not be fetched; try again once relays serve the control plane
core-community-editing-banlist-needs-ban = Editing the banlist needs the BAN permission
core-community-you-do-not-outrank-a-member = You do not outrank a member this ban targets
core-community-private-channel-details-sealed = This private channel's details are sealed under a key this device lacks
core-community-history-read-full-try = This community's history couldn't be read in full; try again in a moment

## Moving a community to new relays, step by step.

# $address: an address as it was typed
core-move-relay-address = { $address } isn't a relay address
core-move-community-needs-least-one = A community needs at least one relay
# $max: a number, the most allowed
core-move-community-lists-most-relays = A community lists at most { $max } relays
# $relay: a relay's address
core-move-answer = { $relay } didn't answer
core-move-account-changed-during-move = The account changed during the move
# $error: a technical reason, often in English
# $relay: a relay's address
# $what: what was asked for, in English
core-move-accept-which-concord-community = { $relay } doesn't accept { $what }, which a Concord community needs, so nothing was changed. It said: { $error }
core-move-rate-limiting-check-relay = It was rate-limiting the check, and a relay that drops what it accepts under load would lose this community's posts when it's busy.
# $load: a sentence of its own, or nothing
# $relay: a relay's address
# $what: what was asked for, in English
core-move-said-saved-test-but = { $relay } said it saved a test { $what } but doesn't serve it back, so nothing was changed.{ $load }
# $kind: a file or event type
# $relay: a relay's address
core-move-only-reached-over-device = { $relay } can only be reached over { $kind }, and this device isn't using { $kind } right now, so it couldn't be checked and nothing was changed.
# $relay: a relay's address
core-move-checking = Checking { $relay }
# $count: a number
core-move-checking-relays = Checking { $count ->
    [one] { $count } relay
   *[other] { $count } relays
}
# $count: a number
core-move-copying-message-history-far = Copying message history ({ $count } so far)
# $error: a technical reason, often in English
# $relay: a relay's address
core-move-refused-some-message-history = { $relay } refused some message history ({ $error })
core-move-catching-up-anything-posted = Catching up on anything posted meanwhile
# $count: a number
core-move-channel-histories-read-whole = { $count ->
    [one] { $count } channel history
   *[other] { $count } channel histories
} couldn't be read whole from the old relays, so some old posts may be missing on the new ones
core-move-reading-community-from-relays = Reading the community from its relays
core-move-none-relays-could-read = None of its relays could be read
# $relay: a relay's address
core-move-read-end = { $relay } couldn't be read to the end
core-move-read-whole = It couldn't be read whole
# $error: a technical reason, often in English
core-move-community-rules-read-full = This community's rules couldn't be read in full ({ $error }), so nothing was changed.
# $count: a number
core-move-parts-community-history-read = { $count ->
    [one] { $count } part
   *[other] { $count } parts
} of the community's history couldn't be read whole from its old relays
core-move-copying-community-structure = Copying the community's structure
# $error: a technical reason, often in English
# $relay: a relay's address
core-move-accept-all-community-events = { $relay } didn't accept all of the community's events, so nothing was changed. It said: { $error }
# $relay: a relay's address
core-move-confirming-holds = Confirming { $relay } holds it
# $relay: a relay's address
core-move-accepted-community-events-but = { $relay } accepted the community's events but doesn't serve them all back, so nothing was changed.
core-move-announcing-move = Announcing the move
core-move-community-no-longer-held = This community is no longer held
core-move-community-relays-changed-while = The community's relays changed while this move ran, so nothing was announced; try again
# $error: a technical reason, often in English
core-move-none-current-relays-took = None of the current relays took the announcement, so members couldn't learn of the move and nothing was changed ({ $error })
# $error: a technical reason, often in English
# $relay: a relay's address
core-move-take-announcement = { $relay } didn't take the announcement ({ $error })
core-move-updating-community-list = Updating your community list

## Posting in a community channel.

core-channel-join-calls-channel = You can't join calls in this channel
core-channel-only-channel-writers-post = Only this channel's writers can post here

## Unlocking and encrypting your data.

core-crypto-account-changed-before-write = The account changed before this write; its key is no longer unlocked
core-crypto-decryption-failed-wrong-key = Decryption failed (wrong key or corrupted data)
core-crypto-account-key-derivation-from = This account's key derivation is from a newer Vector; update to unlock it
core-crypto-account-key-derivation-record = This account's key derivation record is damaged
core-crypto-account-encryption-changed-while = This account's encryption changed while unlocking; enter your PIN again
core-crypto-incorrect-password = Incorrect password
# $error: a technical reason, often in English
core-crypto-encryption-failed = Encryption failed: { $error }
core-crypto-file-too-large-encrypt = File too large to encrypt as one attachment (over 64 GiB)
core-crypto-refusing-download-name-outside = Refusing a download name outside the download folder
core-crypto-file-changed-while-being = The file changed while it was being sent. Attach it again to send it.

## Deleting messages.

core-delete-account-changed-before-deletion = The account changed before the deletion was signed

## Emoji packs.

core-emoji-pack-not-found-any = Pack not found on any relay
# $max: a number, the most allowed
core-emoji-equip-most-packs-remove = You can equip at most { $max } packs. Remove one to add another.
# $max: a number, the most allowed
core-emoji-pack-hold-most-emojis = A pack can hold at most { $max } emojis.
core-emoji-cannot-delete-pack-own = Cannot delete a pack you don't own
core-emoji-account-swapped-during-reorder = Account swapped during reorder — aborted

## Sending messages and files.

# $size: a size, such as 2 MB
core-send-too-large-send-once = Too large to send: { $size } once encrypted, relays take at most 64 KB
# $error: a technical reason, often in English
core-send-read-file = Couldn't read the file: { $error }
core-send-upload-failed = Upload failed:
# $error: a technical reason, often in English
core-send-upload-failed-error = Upload failed: { $error }
core-send-no-relay-accepted-message = No relay accepted the message.
# $max: a number, the most allowed
core-send-failed-send-dm-after = Failed to send DM after { $max } attempts (no relays accepted the gift-wrap)
# $error: a technical reason, often in English
core-send-invalid-npub = Invalid npub: { $error }

## I2P: the router, its SAM bridge and outproxies.

# $port: a port number
core-i2p-service-port-i2p-sam = The service on port { $port } isn't an I2P SAM bridge.
# $port: a port number
# $version: a version number
core-i2p-sam-port = SAM { $version } on port { $port }
core-i2p-not-answering = Not answering
# $seconds: a number of seconds
core-i2p-building = Building… { $seconds }s
core-i2p-off-i2p-only = Off (I2P-Only)
core-i2p-no-outproxy-answering = No outproxy answering
# $name: a name
core-i2p-through = Through { $name }
core-i2p-not-used-yet = Not used yet
# $port: a port number
core-i2p-reach-i2p-router-127 = Can't reach your I2P router at 127.0.0.1:{ $port }.
# $error: a technical reason, often in English
core-i2p-router-open-session = Your router couldn't open a session: { $error }
core-i2p-lost-i2p-session-reconnecting = Lost the I2P session. Reconnecting.
# $code: a status code from the server
# $port: a port number
core-i2p-refused-port = Refused port { $port } ({ $code })
# $code: a status code from the server
core-i2p-busy = Busy ({ $code })
core-i2p-router-closed-session = The router closed the session
core-i2p-router-closed-connection = The router closed the connection
# $port: a port number
core-i2p-no-router-answered-port = No router answered on port { $port }
# $version: a version number
core-i2p-found-router-sam = Found a router. SAM { $version }.
core-i2p-no-router-answered-on-this-port = No router answered on this port.
core-i2p-port-sam-bridge = This port isn't a SAM bridge.
core-i2p-router-asks-sam-username = Your router asks for a SAM username and password.
core-i2p-router-turned-down-sam = Your router turned down the SAM username or password.
core-i2p-router-sam-too-old = Your router's SAM is too old. Vector needs 3.2 or newer.
# $version: a version number
core-i2p-router-speaks-sam-vector = Your router speaks SAM { $version }. Vector needs 3.2 or newer.
core-i2p-looking = Looking…
core-i2p-tunnels = Tunnels
core-i2p-ready = Ready
core-i2p-waiting = Waiting…

## Preparing an image to send.

core-image-svg-images-supported-use = SVG images aren't supported; use PNG, JPEG, GIF or WebP
core-image-not-image-vector-read = Not an image Vector can read (PNG, JPEG, GIF or WebP)
# $format: an image format, such as PNG
core-image-read-strip-metadata = This { $format } couldn't be read to strip its metadata
# $height: a height in pixels
# $width: a width in pixels
core-image-x-too-many-pixels = This image is { $width }x{ $height }: too many pixels to re-encode
# $size: a size, such as 2 MB
core-image-brought-under-kb = This image can't be brought under { $size } KB
# $height: a height in pixels
# $width: a width in pixels
core-image-gif-declares-x-canvas = This GIF declares a { $width }x{ $height } canvas; at most 4096x4096

## Finding where to deliver a direct message.

core-inbox-i2p-only-their-inbox = I2P-Only is on, so their inbox relays are off.
# $network: a network: Tor or I2P
core-inbox-their-inbox-relays-only = Their inbox relays are only reachable over { $network }.
core-inbox-none-their-inbox-relays = None of their inbox relays are reachable on this network.
core-inbox-i2p-only-vector-find = I2P-Only is on, so Vector can't find their inbox relays.
core-inbox-vector-find-their-inbox = Vector can't find their inbox relays on this network.

## Communities, files and messages.

core-app-dms-no-per-message = DMs have no per-message expiry
core-app-only-own-author-withdraw = Only its own author can withdraw a reaction
core-app-empty-image-file = Empty image file
core-app-no-blossom-servers-configured = No Blossom servers configured
core-app-not-vector-invite-link = Not a Vector invite link
core-app-community-been-dissolved-invite = This community has been dissolved — the invite was removed
# $bot: what was typed for the bot
core-app-npub-hex-pubkey = { $bot } isn't an npub or hex pubkey
core-app-empty-file = Empty file
core-app-community-been-dissolved = This community has been dissolved
core-app-reacted-message-not-found = Reacted-to message not found
core-app-message-not-found-already = Message not found (already deleted?)
core-app-no-such-channel-community = No such channel in this community
core-app-private-channel-no-key = This private channel has no key yet (awaiting rekey delivery)
core-app-moderation-tools-require-concord = Moderation tools require a Concord v2 community
core-app-policies-require-concord-v2 = Policies require a Concord v2 community
core-app-policy-exceeds-maximum-document = Policy exceeds the maximum document size
core-app-none-relays-answered = None of your relays answered
core-app-founding-requires-concord-v2 = Re-founding requires a Concord v2 community
core-app-kicking-requires-concord-v2 = Kicking requires a Concord v2 community
core-app-pins-only-available-concord = Pins are only available in Concord v2 communities
core-app-message-not-pinned = That message is not pinned
core-app-pin-carries-no-attachment = This pin carries no attachment
core-app-roles-need-concord-v2 = Roles need a Concord v2 community
core-app-new-roles-channel-access = On the new roles, channel access is set on the channel
core-app-needs-concord-v2-community = This needs a Concord v2 community
core-app-rescuing-requires-concord-v2 = Rescuing requires a Concord v2 community
# $error: a technical reason, often in English
core-app-community-image-url-refused = Community image URL refused: { $error }
core-app-community-image-too-large = Community image too large
core-app-community-image-failed-integrity = Community image failed its integrity check
# $relay: a relay's address
core-app-relay-address = { $relay } isn't a relay address
# $relay: a relay's address
core-app-voice-relay-https-address = { $relay }: a voice relay is an https:// address
# $max: a number, the most allowed
core-app-most-voice-relays = At most { $max } voice relays
core-app-join-leave-messages-go = Join and leave messages go to a chat channel
core-app-new-members-start-public = New members start in a public text or forum channel
# $error: a technical reason, often in English
core-app-send-uncompressed-share = { $error }; send it uncompressed to share it as it is
# $error: a technical reason, often in English
core-app-invalid-nsec = Invalid nsec: { $error }

## Files kept on this device.

core-media-file-changed-while-being = The file changed while it was being moved in
# $error: a technical reason, often in English
core-media-keep-file = Couldn't keep the file: { $error }

## Web addresses Vector refuses to visit.

core-net-only-http-urls-allowed = Only HTTP(S) URLs are allowed
core-net-private-internal-ip-addresses = Private/internal IP addresses are not allowed
core-net-local-hostnames-not-allowed = Local hostnames are not allowed
core-net-vector-open-local-proxy = Vector couldn't open its local proxy.
core-net-network-changed = The network changed.

## Previews of Nostr posts in a message.

core-embed-not-post-article-video = Not a post, article or video reference
core-embed-not-found-any-relay = Not found on any relay
core-embed-repost-say-what-reposted = This repost doesn't say what it reposted
core-embed-reposted-post-wasn-found = The reposted post wasn't found
# $kind: a file or event type
core-embed-unsupported-nostr-event-kind = Unsupported Nostr event (kind { $kind })

## Notifications when a message preview is hidden.

core-notify-sent-message = Sent a message
core-notify-sent-you-a-message = Sent you a message
core-notify-new-message = New message
core-notify-received-message = You received a message

## Pinned chats.

# $max: a number, the most allowed
core-pins-pin-up-chats-unpin = You can pin up to { $max } chats — unpin one first
core-pins-chat-not-pinned = That chat is not pinned

## Your profile.

core-profile-relays-one-vector-read = Your profile on the relays isn't one Vector can read, so it was left as it is
core-profile-find-profile-relays = Couldn't find your profile on your relays
core-profile-read-profile-from-relays = Couldn't read your profile from your relays

## The community rail and its folders.

core-rail-arranged-newer-version-vector = This rail was arranged by a newer version of Vector
# $max: a number, the most allowed
core-rail-holds-most-folders = A rail holds at most { $max } folders
core-rail-no-such-folder = No such folder
core-rail-no-such-row = No such row
core-rail-no-such-community = No such community

## Downloading files.

core-download-file-exceeds-maximum-download = File exceeds the maximum download size
core-download-cancelled = Download cancelled
core-download-failed-prepare-download = Failed to prepare the download
core-download-failed-save-download = Failed to save the download

## Disappearing messages.

core-expiry-message-already-expired = That message has already expired

## Remote signers.

core-signer-network-changed-try-again = The network changed. Try again.
core-signer-connected-yet = Your signer isn't connected yet.
# $error: a technical reason, often in English
core-signer-invalid-bunker-url = Invalid bunker URL: { $error }

## Start menu pins.

core-start-pinned = That can't be pinned
core-start-pins-were-arranged-newer = Your pins were arranged by a newer Vector; update to change them
# $max: a number, the most allowed
core-start-holds-up-pins = Start holds up to { $max } pins

## Settings synced between your devices.

# $max: a number, the most allowed
core-sync-archive-full-chats = The archive is full ({ $max } chats)
core-sync-still-syncing-settings-try = Still syncing your settings, try again in a moment
core-sync-list-full-would-no = This list is full: it would no longer fit one event

## Tor: connecting, bridges and onion services.

core-tor-exit-timed-out-reaching = The Tor exit timed out reaching this server.
core-tor-timed-out-building-path = Tor timed out building a path. Try again.
core-tor-server-refused-connection-from = This server refused the connection from Tor.
core-tor-no-tor-exit-allows = No Tor exit allows this port.
core-tor-exit-find-server = The Tor exit couldn't find this server.
core-tor-exit-reach-server = The Tor exit couldn't reach this server.
core-tor-server-closed-connection = This server closed the connection.
core-tor-relays-busy-try-again = Tor relays are busy. Try again.
core-tor-onion-service-found = This onion service can't be found.
core-tor-onion-service-online-right = This onion service isn't online right now.
core-tor-reach-onion-service = Couldn't reach this onion service.
core-tor-onion-service-answered-incorrectly = This onion service answered incorrectly.
core-tor-onion-service-needs-key = This onion service needs a key Vector doesn't have.
core-tor-onion-address-valid = That onion address isn't valid.
core-tor-reach-server = Tor couldn't reach this server.
core-tor-needs-data-directories = Tor needs its data directories.
# $seconds: a number of seconds
core-tor-timed-out-after = Tor timed out after { $seconds }s.
core-tor-closed-connection = Tor closed the connection.
core-tor-program-port-tor = The program on this port isn't Tor.
core-tor-refused-vector-connection = Tor refused Vector's connection.
core-tor-answered-incorrectly = Tor answered incorrectly.
core-tor-exit-policy-refused-connection = Tor's exit policy refused this connection.
core-tor-exit-reach-server-network = The Tor exit couldn't reach this server's network.
core-tor-onion-service-descriptor-invalid = This onion service's descriptor is invalid.
# $port: a port number
core-tor-waiting-tor-port-open = Waiting for Tor on port { $port }. Open Tor Browser or start tor.
core-tor-waiting-tor-open-tor = Waiting for Tor. Open Tor Browser or start tor.
core-tor-vector-built-without-tor = Vector was built without the `tor` feature.
core-tor-install-via-brew-install = Install via `brew install obfs4proxy`.
core-tor-install-via-apt-install = Install via `apt install obfs4proxy` (or your distro's package manager).
core-tor-download-obfs4proxy-exe-from = Download `obfs4proxy.exe` from torproject.org and add it to PATH.
core-tor-obfs4-supported-android-yet = Obfs4 isn't supported on Android yet.
core-tor-install-obfs4proxy-platform-add = Install `obfs4proxy` for your platform and add it to PATH.
# $hint: more about what to do, a sentence of its own
core-tor-obfs4-bridges-configured-but = Obfs4 bridges configured but `obfs4proxy` (or `lyrebird`) was not found. { $hint }
core-tor-not-running = Tor not running

## Moving your account to another device.

core-transfer-code-look-right-number = That code doesn't look right. It's a number and three words, like 7-orbit-lemon-stage.
# $word: the word as it was typed
core-transfer-one-code-words-check = "{ $word }" isn't one of the code words. Check the spelling.
core-transfer-account-no-key-device = This account has no key on this device to move
core-transfer-newer-transfer-replaced-one = A newer transfer replaced this one
core-transfer-no-transfer-running = No transfer is running
core-transfer-codes-match-check-code = The codes didn't match. Check the code and try again.
core-transfer-another-device-tried-use = Another device tried to use this code, so the transfer stopped. Start again with a new code.
core-transfer-both-devices-signed-new = Both devices are signed in. On the new device, choose Sign in with another device on the sign in screen.
core-transfer-neither-device-signed-signed = Neither device is signed in. On your signed-in device, open Settings and choose Sign in on another device.
core-transfer-other-device-running-different = The other device is running a different version of Vector. Update both and try again.
core-transfer-declined-other-device = The transfer was declined on your other device.
core-transfer-account-arrive-intact-nothing = The account didn't arrive intact, so nothing was saved. Try again.
core-transfer-number-entered-wrong-too = The number was entered wrong too many times. Start again.
core-transfer-something-went-wrong-between = Something went wrong between the devices. Try again.
core-transfer-both-devices-same-side = Both devices are on the same side of the transfer. One must be signed in and the other new.
core-transfer-the-number-was-entered-wrong-too = The number was entered wrong too many times on your other device. Start again.
core-transfer-other-device-stopped-waiting = Your other device stopped waiting. Start again.
core-transfer-cancelled-other-device = The transfer was cancelled on your other device.
core-transfer-lost-connection-relays-check = Lost the connection to the relays. Check your connection and that the device's clock is right.
core-transfer-nothing-happened-while-transfer = Nothing happened for a while, so the transfer stopped. Start again when you're ready.
core-transfer-code-expired-start-again = The code expired. Start again for a new one.
core-transfer-reach-relays-check-connection = Couldn't reach the relays. Check your connection and try again.

## How Vector connects: networks, addresses and their status.

core-network-valid-server-address = That isn't a valid server address.
core-network-enter-b32-i2p-address = Enter a .b32.i2p address.
core-network-enter-onion-address = Enter a .onion address.
# $kind: a file or event type
core-network-addresses-supported-yet = { $kind } addresses aren't supported yet.
core-network-already-running = A network is already running.
core-network-add-up-8-outproxies = You can add up to 8 outproxies.
core-network-enter-name-up-32 = Enter a name up to 32 characters.
core-network-enter-b32-i2p-i2p = Enter a .b32.i2p or .i2p address.
core-network-outproxy-already-list = This outproxy is already in the list.
core-network-keep-one-outproxy-turn = Keep one outproxy on, or turn on I2P-Only.
core-network-save-applies-until-vector = Couldn't save this. It applies until Vector restarts.
core-network-i2p-address = I2P address
core-network-vector-still-connecting = Vector is still connecting.
# $network: a network: Tor or I2P
core-network-still-connecting = { $network } is still connecting.
# $port: a port number
core-network-cant-reach-tor = Can't reach Tor at 127.0.0.1:{ $port }.
core-network-i2p-name-router-address = This I2P name isn't in your router's address book. Use its .b32.i2p address.
core-network-i2p-address-online-right = This I2P address isn't online right now.
# $name: a name
core-network-outproxy-reach-server = The { $name } outproxy couldn't reach this server.
# $port: a port number
core-network-no-outproxy-here-allows = No outproxy here allows port { $port }.
core-network-timed-out = Timed out.
core-network-use-wss-ws-i2p = Use wss://, or ws:// for an .i2p or .onion address.
core-network-relay-url-must-include = Relay URL must include a host
# $network: a network: Tor or I2P
core-network-server-only-reachable-over = This server is only reachable over { $network }.
# $network: a network: Tor or I2P
core-network-vector-reach-addresses-yet = Vector can't reach { $network } addresses yet.
core-network-i2p-only-server-off = I2P-Only is on, so this server is off.
core-network-i2p-reach-bare-ip = I2P can't reach a bare IP address.
core-network-local-addresses-reachable-over = Local addresses aren't reachable over I2P.
# $detail: more detail, often in English
core-network-blocked = Blocked: { $detail }
# $network: a network: Tor or I2P
core-network-inside = Inside { $network }
# $address: an address as it was typed
core-network-through = Through its { $address }
core-network-through-tor = Through Tor
# $name: a name
core-network-through-outproxy = Through the { $name } outproxy
core-network-through-an-outproxy = Through an outproxy
# $network: a network: Tor or I2P
core-network-build-include = This build doesn't include { $network }.
core-network-look-up-i2p-only = Can't look it up in I2P-Only mode.
# $noun: a kind of address, such as onion address
core-network-relay-list = This relay doesn't list an { $noun }.
# $noun: a kind of address, such as onion address
core-network-relay-lists = This relay lists an { $noun }.
# $host: a server's address
core-network-checked-serves = Checked. It serves { $host }.
# $host: a server's address
core-network-address-serve-vector-use = This address doesn't serve { $host }. Vector won't use it.
# $network: a network: Tor or I2P
core-network-connect-check-address = Connect to { $network } to check this address.
# $address: an address as it was typed
core-network-server-no-yet = This server has no { $address } yet.
core-network-cancelled = Cancelled.

## Chat wallpapers.

# $max: a number, the most allowed
# $size: a size, such as 2 MB
core-wallpaper-image-too-large-mb = Image is too large ({ $size } MB). Wallpapers max out at { $max } MB.
core-wallpaper-wallpapers-must-image-files = Wallpapers must be image files.
core-wallpaper-svg-too-complex-use = That SVG is too complex to use as a wallpaper.
core-wallpaper-no-wallpaper-preview-publish = No wallpaper preview to publish. Pick an image first.
# $error: a technical reason, often in English
core-wallpaper-upload-failed = Wallpaper upload failed: { $error }
# $error: a technical reason, often in English
core-wallpaper-send-wallpaper-check-relays = Couldn't send the wallpaper. Check that the relays you and your contact share are reachable, then try again. ({ $error })
# $error: a technical reason, often in English
core-wallpaper-download-failed = Wallpaper download failed: { $error }
# $error: a technical reason, often in English
core-wallpaper-remove-wallpaper-check-relays = Couldn't remove the wallpaper. Check that the relays you and your contact share are reachable, then try again. ({ $error })

## What a Mini App may ask for.

core-permission-read-clipboard = Read Clipboard
core-permission-write-clipboard = Write Clipboard
core-permission-autoplay-media = Autoplay Media
core-permission-screen-capture = Screen Capture
core-permission-midi-devices = MIDI Devices
core-permission-keep-screen = Keep Screen On
core-permission-speaker-selection = Speaker Selection
core-permission-light-sensor = Light Sensor
core-permission-access-microphone-voice-chat = Access your microphone for voice chat or recording
core-permission-access-camera-video-calls = Access your camera for video calls or photos
core-permission-access-device-location = Access your device location
core-permission-read-text-from-clipboard = Read text from your clipboard
core-permission-copy-text-clipboard = Copy text to your clipboard
core-permission-enter-fullscreen-mode = Enter fullscreen mode
core-permission-automatically-play-audio-video = Automatically play audio and video
core-permission-capture-screen-window = Capture your screen or window
core-permission-connect-midi-instruments-controllers = Connect to MIDI instruments and controllers
core-permission-play-video-floating-window = Play video in a floating window
core-permission-prevent-screen-from-sleeping = Prevent your screen from sleeping
core-permission-choose-which-speaker-use = Choose which speaker to use for audio
core-permission-detect-device-acceleration-movement = Detect device acceleration and movement
core-permission-detect-device-rotation-orientation = Detect device rotation and orientation
core-permission-access-compass-magnetic-field = Access compass and magnetic field data
core-permission-detect-ambient-light-levels = Detect ambient light levels
core-permission-connect-bluetooth-devices = Connect to Bluetooth devices
core-permission-camera = Camera
core-permission-location = Location
core-permission-fullscreen = Fullscreen
core-permission-accelerometer = Accelerometer
core-permission-gyroscope = Gyroscope
core-permission-magnetometer = Magnetometer
core-permission-bluetooth = Bluetooth

## Mini Apps.

core-miniapp-listing-keeps-app-id = A listing keeps its App ID
core-miniapp-only-publisher-edit-listing = Only its publisher can edit this listing
# $error: a technical reason, often in English
core-miniapp-realtime-unavailable = Realtime unavailable: { $error }
core-miniapp-not-community-channel = Not a Community channel
# $error: a technical reason, often in English
core-miniapp-invalid-mini-app = Invalid mini app: { $error }
core-miniapp-invalid-manifest-too-large = Invalid manifest: too large
# $error: a technical reason, often in English
core-miniapp-invalid-manifest = Invalid manifest: { $error }
