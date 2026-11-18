### Settings: its pages, their sections, and the rows in them.

## Pages in the side navigation

settings-page-appearance = Appearance
settings-page-audio = Audio
settings-page-notifications = Notifications
settings-page-network = Network
settings-page-storage = Storage
settings-page-privacy = Privacy
settings-page-security = Security
settings-page-updates = Updates
settings-page-danger = Danger Zone

## Sections inside the pages. Each section's .keys are the words a search finds it by, separated by spaces.

settings-section-voice = Voice
    .keys = voice whisper transcribe transcription translate model download
settings-section-privacy = Privacy
    .keys = web previews video players youtube embeds url tracking typing indicators proxy media
settings-section-blocked = Blocked Users
    .keys = blocked users block unblock
settings-section-display = Display
    .keys = theme colour color vector satoshi chatstr cosmic purple neon xmr display image types background wallpaper rich composer emoticon suggestions time suggestions countdown autocorrect send on enter return key floating player main menu navbar navigation pins sidebar tabs
settings-section-notifications = Notifications
    .keys = mute sounds everyone pings content privacy sound prelude techno custom
settings-section-relays = Nostr Relays
    .keys = network nostr relays add custom
settings-section-servers = Media Servers
    .keys = network media servers blossom upload add custom
settings-section-clock = Clock
    .keys = clock time date decentralised decentralized device relays slow fast timestamp
settings-section-storage = Storage
    .keys = storage usage clear auto-download download limit gallery show save keep files unencrypted media video quality compression compress
settings-section-security = Security
    .keys = security pin password biometric signer bunker encryption key
settings-section-updates = Updates
    .keys = updates version release changelog download install
settings-section-advanced = Advanced
    .keys = advanced mode developer bot ids copy id identifier
settings-section-logs = Logs
    .keys = copy logs crash debug
settings-section-logout = Logout
    .keys = logout log out sign out
settings-section-calls = Calls
    .keys = calls microphone speaker automatic gain echo cancellation noise suppression test mic device
settings-section-streamer = Streamer Mode
    .keys = streamer stream streaming live obs twitch screen share hide names pictures notifications
settings-section-routing = Routing
    .keys = routing network tor i2p onion standard clearnet bridges obfs4 circuit outproxy sam router anonymity

## Switches, with the headings that group them on the Display page

settings-group-navigation = Navigation
settings-group-chat = Chat
settings-group-composer = Composer
settings-group-media = Media
settings-group-chat-list = Chat List
settings-group-appearance = Appearance
settings-theme = Theme

settings-toggle-ws-nav = Main Menu
settings-toggle-display-image-types = Display Image Types
settings-toggle-chat-bg-enabled = Background Wallpaper
settings-toggle-emoticon-suggestions = Emoticon Suggestions
settings-toggle-time-suggestions = Time Suggestions
settings-toggle-autocorrect = Autocorrect
settings-toggle-send-on-enter = Send on Enter
settings-toggle-floating-player = Floating Player
settings-toggle-cc-rail-hidden = Hide Community Rail
settings-toggle-web-previews = Display Web Previews
settings-toggle-strip-tracking = Prevent URL Tracking
settings-toggle-send-typing-indicators = Send Typing Indicators
settings-toggle-privacy-proxy-media = Proxy Previews & Media
settings-toggle-players = Video Players
settings-toggle-streamer-on = Streamer Mode
settings-toggle-stream-consent = Show me on streams
settings-toggle-streamer-wallpapers = Hide chat wallpapers
settings-toggle-advanced = Advanced Mode
settings-toggle-notif-global-mute = Mute All Notification Sounds
settings-toggle-notif-mute-everyone = Mute @Everyone Pings
settings-toggle-auto-download-enabled = Auto-Download Media
settings-toggle-keep-media = Keep Files on This Device
settings-toggle-keep-unencrypted = Keep Unencrypted
settings-toggle-clock-decentralised = Decentralised Clock

## The (i) beside a row

# What a screen reader says for an (i) with no row beside it.
settings-more-info = More info
# What a screen reader says for a row's (i). $name is the row's label.
settings-about = About { $name }

## Streamer Mode

settings-streamer = Streamer Mode
settings-streamer-also-hide = While streaming, also hide
settings-streamer-hide-none = Nothing extra
settings-streamer-hide-content = Messages
settings-streamer-hide-sender = Names
settings-streamer-hide-all = Names and messages
# What a notification shows while streaming. $name and $message are the two parts below.
settings-streamer-caption = Notifications while streaming: { $name }, { $message }
settings-streamer-name-hidden = name hidden
settings-streamer-name-shown = name shown if they allow streams
settings-streamer-message-hidden = message hidden
settings-streamer-message-shown = message shown

## Blocked users

settings-blocked-none = No blocked users
settings-unblocked = User unblocked
settings-unblock = Unblock

## Notifications

settings-notif-privacy = Content Privacy
settings-notif-privacy-full = Show sender and message
settings-notif-privacy-hide-content = Hide message
settings-notif-privacy-hide-sender = Hide sender
settings-notif-privacy-hide-all = Hide sender and message
settings-notif-sound = Notification Sound
settings-notif-sound-none = None
settings-notif-sound-custom = Custom...
settings-notif-sound-preview = Preview Sound
settings-notif-sound-unknown = Unknown file
settings-notif-sound-unpicked = No file selected
settings-notif-sound-change = Click to change sound
settings-notif-sound-remove = Remove custom sound

## Danger Zone

settings-logout = Logout
settings-copy-logs = Copy Logs
settings-copy-logs-button = Copy

## Settings list

settings-title = Settings
settings-search-none = No matches
settings-copy-link = Copy Link
settings-link-donate = Donate
settings-link-docs = Docs
settings-link-privacy = Privacy Policy

## Storage. Each kind of file has a name, the title of the popup that deletes it, and the
## popup's two paragraphs. In .text, $size is how much space they take up, like "12 MB".

# .text: $size is how much space they take up, like "12 MB".
settings-storage-images = Images
    .ask = Delete all Images?
    .text = This will delete { $size } of downloaded images from this device.
    .again = You can download them again from their chats later, if they are still available.
# .text: $size is how much space they take up, like "12 MB".
settings-storage-video = Video
    .ask = Delete all Videos?
    .text = This will delete { $size } of downloaded videos from this device.
    .again = You can download them again from their chats later, if they are still available.
# .text: $size is how much space they take up, like "12 MB".
settings-storage-audio = Audio
    .ask = Delete all Audio?
    .text = This will delete { $size } of downloaded audio and voice messages from this device.
    .again = You can download them again from their chats later, if they are still available.
# .text: $size is how much space they take up, like "12 MB".
settings-storage-apps = Apps
    .ask = Delete all Mini Apps?
    .text = This will delete { $size } of downloaded Mini Apps from this device.
    .again = You can download them again from their chats later, if they are still available.
# .text: $size is how much space they take up, like "12 MB".
settings-storage-ai = AI
    .ask = Delete AI Models?
    .text = This will delete { $size } of downloaded AI models from this device.
    .again = Voice transcription will download a model again when needed.
# .text: $size is how much space they take up, like "12 MB".
settings-storage-cache = Cache
    .ask = Clear the Cache?
    .text = This will delete { $size } of cached avatars, banners and images.
    .again = Vector rebuilds this cache automatically over time.
# .text: $size is how much space they take up, like "12 MB".
settings-storage-files = Files
    .ask = Delete all Files?
    .text = This will delete { $size } of downloaded files from this device.
    .again = You can download them again from their chats later, if they are still available.

# The chart's caption for one kind of file. $name is its name (Images, Video...); $percent is its
# share of the total, a number or "<1".
settings-storage-slice = { $name } · { $percent }%
settings-storage-total = Total
settings-storage-delete = Delete
settings-storage-deleting = Deleting...
# A size limit. $count is a number of megabytes.
settings-storage-megabytes = { $count } MB
settings-auto-download-limit = Auto-Download Limit
settings-video-quality = Video Quality
settings-video-quality-small = Small
settings-video-quality-balanced = Balanced
settings-video-quality-high = High
settings-video-quality-original = Original
settings-storage-decrypting = Decrypting your kept files
settings-storage-moving = Moving your files into Vector
settings-clear-storage = Clear Storage
settings-storage-clear = Clear
settings-storage-clearing = Clearing...

## Relays and media servers

settings-add-relay = Add Custom Relay
settings-add-relay-button = Add Relay
settings-add-server = Add Custom Server
settings-add-server-button = Add Server
settings-origin-default = Default
settings-origin-custom = Custom
# How quickly a server answered. $ms is a number of milliseconds.
settings-latency = { $ms } ms
settings-relay-initialized = Initialized
settings-relay-pending = Pending
settings-relay-connecting = Connecting
settings-relay-connected = Connected
settings-relay-disconnected = Not Connected
settings-relay-terminated = Terminated
settings-relay-shutdown = Shutdown
settings-relay-banned = Banned
settings-relay-sleeping = Sleeping
settings-relay-disabled = Disabled
settings-server-online = Online
settings-server-offline = Offline
settings-server-maxed = Maxed
settings-server-no-access = No access
settings-server-untested = Untested
settings-server-disabled = Disabled

## Clock

settings-clock-checking = Checking the time with your relays…
settings-clock-too-few = Fewer than three of your relays agreed on the time, so this device’s clock is in use.
settings-clock-matches = This device’s clock matches your relays.
# $span is how far apart the clocks are, like "5 minutes".
settings-clock-slow-fixed = This device’s clock is { $span } slow. Vector is using your relays’ time.
# $span is how far apart the clocks are, like "5 minutes".
settings-clock-fast-fixed = This device’s clock is { $span } fast. Vector is using your relays’ time.
# $span is how far apart the clocks are, like "5 minutes".
settings-clock-slow = Your relays say this device’s clock is { $span } slow.
# $span is how far apart the clocks are, like "5 minutes".
settings-clock-fast = Your relays say this device’s clock is { $span } fast.

## Security

settings-signer = Remote Signer
settings-signer-reauthorize = Re-authorize
settings-signer-key = Signer key
settings-signer-note = Your identity key lives on your signer app. Vector only holds a device pairing key.
settings-encryption = Local Encryption
settings-change-password = Change Password
settings-change-pin = Change PIN
settings-change = Change
settings-transfer = Sign in on Another Device
settings-transfer-start = Start
settings-security-advanced = Advanced
settings-private-keys = Private Keys
settings-private-keys-show = Show

## Updates

settings-update-current = Current Version
# $version is the newest version, like "0.5.2".
settings-update-outdated = Your client is currently out of date. The latest version is { $version }.
settings-update-latest = Your client is up to date.
    .more = It is currently on the latest release.
settings-update-preview = This is a Preview Build.
    .more = It is released ahead of the official version. Preview Builds are public prototypes used to test new features and surface bugs, so feedback from users helps improve the final release. Expect bugs, unfinished features, and occasional instability.
settings-update-whats-new = What's New:
settings-update-beta = Beta Updates
settings-update-check = Check for Updates
settings-update-download = Download Update
settings-update-button-checking = Checking...
settings-update-downloading = Downloading...
settings-update-checking = Checking for updates...
settings-update-ready = Update ready! Restart to apply.
settings-update-no-newer = No newer build yet. You'll be offered the official build as soon as it releases.
settings-update-restart = Restart Now
settings-update-check-failed = Failed to check for updates
settings-update-download-stopped = The download stopped
# The banner across the top. $version is the new version, like "0.5.3".
settings-update-banner = New Vector Update Available ({ $version }). Download Now

## Voice

settings-voice-translate = Auto-Translate
settings-voice-no-translate = Selected model does not support translation
settings-voice-transcribe = Auto-Transcribe
settings-voice-model = Whisper Model
# $model is the model's name.
settings-voice-model-low-ram = { $model } (Insufficient RAM)
# $model is the model's name.
settings-voice-model-recommended = { $model } [Recommended]
# $progress is how far the download is, like "45%" or "12.3/140.0 MB".
settings-voice-downloading = Downloading... { $progress }
settings-voice-ready = Vector AI is ready
settings-voice-model-deleted = Model Deleted
settings-voice-not-downloaded = Model not downloaded
settings-voice-cancel-download = Cancel Download
# $size is the model's size, like "142 MB".
settings-voice-download = Download Model ({ $size })

## Calls

settings-device-default = Default
settings-device-system-default = System Default
# The device the system picks; $device is what it is now.
settings-device-system-default-named = System Default ({ $device })
settings-microphone = Microphone
settings-speaker = Speaker
settings-mic-volume = Microphone Volume
settings-speaker-volume = Speaker Volume
settings-mic-on-call = On a call
settings-mic-test = Test Mic
settings-mic-test-stop = Stop Test
settings-group-microphone = Microphone
settings-group-connection = Connection
settings-toggle-auto-gain = Automatic Gain
settings-toggle-echo-cancel = Echo Cancellation
settings-toggle-noise-suppress = Noise Suppression
settings-toggle-direct = Direct Connection
