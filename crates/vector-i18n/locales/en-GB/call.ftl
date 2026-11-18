### A 1:1 call: its window and pill, the controls, and the connection panel.

## What the call is doing, under the other person's name

call-calling = Calling…
call-incoming = Incoming call
call-incoming-video = Incoming video call
call-connecting = Connecting…
call-peer-away = Waiting for them to come back…
call-reconnecting = Reconnecting…
# While the other person is muted. $status is the call's status or its running clock.
call-status-muted = { $status } · muted

## Why a call ended

call-ended = Call ended
call-ended-declined = Declined
call-ended-busy = Busy
call-ended-no-answer = No answer
call-ended-missed = Missed call
call-ended-connect-failed = Could not connect
call-ended-disconnected = Connection lost
call-ended-audio-failed = Microphone unavailable
call-ended-send-failed = Could not reach the relays
call-ended-elsewhere = Answered on another device

## Controls

call-mic-off = Microphone access is off
call-mic-denied-muted = You're muted: Vector doesn't have access to your microphone.
call-no-video = They cannot receive video from this device
call-camera-failed = Could not open the camera
call-screen-failed = Could not share the screen
call-not-in-call = Not in a call

## Asking for the microphone, where the system refused it. Each text names where the system
## keeps the setting, as that system words it.

call-mic-access-title = Microphone Access
call-mic-access-text-macos = Vector doesn't have access to your microphone. Allow it in System Settings → Privacy & Security → Microphone, then try again.
call-mic-access-text-windows = Vector doesn't have access to your microphone. Allow it in Settings → Privacy → Microphone, then try again.
call-mic-access-text-other = Vector doesn't have access to your microphone. Allow it in your device's app settings, then try again.
call-mic-open-settings = Open Settings
call-mute = Mute microphone
call-unmute = Unmute microphone
call-hang-up = Hang up
call-accept = Accept
call-decline = Decline
call-open-channel = Open the channel
call-their-volume = Their volume

## How good the connection is

call-quality-excellent = Excellent
call-quality-good = Good
call-quality-fair = Fair
call-quality-poor = Poor

## The connection panel. Section headings are shown in capitals.

call-your-voice = Your voice
call-processing = Voice processing
call-auto-gain = Automatic gain
call-echo-cancel = Echo cancellation
call-noise-suppress = Noise suppression
call-connection = Connection
call-quality = Quality
call-measuring = Measuring…
call-delay = Delay
# A round trip. $ms is whole milliseconds.
call-delay-ms = { $ms } ms
call-via-relay = via relay
call-direct = direct
call-lost = Lost audio
    .tip = Moments the speaker had to fill in because the audio was late or missing
call-lost-none = none
call-lost-under-one = under 1%
call-bitrate = Bitrate
    .tip = How much data your voice uses. It rises on a clean connection and drops when packets are being lost
# $kbps is whole kilobits a second.
call-bitrate-kbps = { $kbps } kbit/s
call-bitrate-lowered = lowered, the network is dropping packets
call-bitrate-full = full quality

## The connection graph and the moment under the pointer

# Before the graph has its first points.
call-graph-measuring = Measuring the connection…
# The delay line's key. $ms is the graph's top, in whole milliseconds.
call-graph-delay = delay, up to { $ms } ms
call-graph-lost = lost audio
call-graph-now = now
# How long ago a moment on the graph was. $seconds is a whole number.
call-graph-ago = { $seconds }s ago
call-graph-lost-none = nothing lost
call-graph-lost-under-one = under 1% lost
# $percent is the share of audio lost at that moment.
call-graph-lost-percent = { $percent }% lost

## Cameras and screens

call-camera-off = Turn the camera off
call-camera-on = Turn the camera on
call-screen-stop = Stop sharing the screen
call-screen-share = Share the screen
call-waiting-camera = Waiting for their camera…
call-waiting-screen = Waiting for their screen…
call-fill-window = Fill the window
call-fill-with-camera = Fill the window with their camera
# $name is the other person's name.
call-sharing-screen = { $name } is sharing a screen
call-their-camera-off = Their camera is off
call-restore = Back to the call window (Esc)
# Swaps which of their pictures fills the window.
call-swap = Swap

## The panel's picture blocks. Headings are shown in capitals.

call-camera-quality = Camera quality
call-screen-quality = Screenshare quality
# What a picture of ours is sending. $size is its width×height, like 1280×720.
call-video-stats = { $size } · { $fps } fps · { $kbps } kbit/s
# What one of their pictures arrives as. $size is its width×height, like 1280×720.
call-video-size = { $size } · { $fps } fps
call-frame-rate = Frame rate
# The quality or frame rate the call picks for itself as the connection changes.
call-adaptive = Adaptive
# A frame rate. $fps is frames a second.
call-fps = { $fps } fps
call-rung-low = Low
call-rung-medium = Medium
call-rung-high = High
call-rung-best = Best
call-rung-ultra = Ultra
# The shared window, and a button to pick another.
call-window = Window
call-window-change = Change…
# Whether the shared window's sound goes with it.
call-sound = Sound
call-sound-sharing = Sharing
call-sound-not-shared = Not shared
call-sound-permission = System permission required
call-sound-unavailable = Not offered by this window
call-their-camera = Their camera
call-their-screen = Their screen
