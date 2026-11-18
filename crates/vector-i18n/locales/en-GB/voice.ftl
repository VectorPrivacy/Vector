### Voice Channels: a participant's right-click menu.

voice-mute-for-me = Mute for Me
voice-unmute-for-me = Unmute for Me
voice-volume = Volume

## A moderator's rows

voice-server-mute = Server Mute
voice-server-unmute = Server Unmute
voice-server-deafen = Server Deafen
voice-server-undeafen = Server Undeafen
# Moves the participant to another voice channel. $channel is its name.
voice-move-to = Move to { $channel }
voice-disconnect = Disconnect

## A participant's state, as a small icon's tooltip

voice-sharing-their-screen = Sharing their screen
voice-camera-on = Camera on
voice-muted = Muted
voice-muted-by-mod = Muted by a moderator
voice-deafened = Deafened
voice-deafened-by-mod = Deafened by a moderator
# A tag on someone sharing their screen. Shown in capitals.
voice-live = Live
voice-you = You
voice-note-elsewhere = another app
voice-note-connecting = connecting
voice-elsewhere-tip = In this call on another app: you can't hear each other
# $ms is the round trip in whole milliseconds.
voice-path-direct = { $ms } ms, direct
# $ms is the round trip in whole milliseconds.
voice-path-relay = { $ms } ms, through a relay
voice-hide-their-screen = Hide their screen
voice-show-their-screen = Show their screen

## Your controls. $key is the keyboard shortcut, like "Ctrl+Shift+M".

# $key is the keyboard shortcut.
voice-mute-key = Mute ({ $key })
# $key is the keyboard shortcut.
voice-unmute-key = Unmute ({ $key })
# $key is the keyboard shortcut.
voice-deafen-key = Deafen ({ $key })
# $key is the keyboard shortcut.
voice-undeafen-key = Undeafen ({ $key })
voice-camera-turn-off = Turn your camera off
voice-camera-turn-on = Turn your camera on
voice-screen-stop = Stop sharing your screen
voice-screen-share = Share your screen
voice-leave = Leave the call
voice-join = Join Voice
voice-joining = Joining…

## A shared screen

voice-your-screen = Your screen
# $name is whose screen it is.
voice-their-screen = { $name }'s screen
voice-smaller = Smaller
voice-bigger = Bigger
voice-hide = Hide

## The channel's status line, and the pill while you're elsewhere

voice-nobody = Nobody is here yet
# $count is how many people are in the call.
voice-in-call = { $count ->
    [one] { $count } in the call
   *[other] { $count } in the call
}
voice-sharing = Sharing your screen
# $count is how many people are watching your screen.
voice-sharing-watched = { $count ->
    [one] Sharing your screen · { $count } watching
   *[other] Sharing your screen · { $count } watching
}
voice-no-mic = No microphone access: others can't hear you
voice-ping-tip = Round trip to the slowest link in the call
# $ms is the round trip in whole milliseconds.
voice-ping-bar = { $ms } ms to the slowest link
voice-no-audio = Without audio
# $time is how long you've been in the call, like "4:05".
voice-waiting = Waiting for others · { $time }
# $count is how many people are in the call, and $time how long you've been in it.
voice-in-call-time = { $count ->
    [one] { $count } in the call · { $time }
   *[other] { $count } in the call · { $time }
}
# Stands in for a voice channel's name before it has loaded.
voice-fallback-name = Voice
