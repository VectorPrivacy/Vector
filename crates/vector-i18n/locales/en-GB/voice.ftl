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
voice-stage-compact = Show just faces
voice-stage-minimise = Fold into one row
voice-stage-expand = Show the whole call
voice-note-elsewhere = another app
voice-note-connecting = connecting
voice-elsewhere-tip = In this call on another app: you can't hear each other
# A member's link to you. $ms is the round trip in whole milliseconds, $loss the share of packets lost.
voice-path-direct = { $ms } ms, { NUMBER($loss, minimumFractionDigits: 1, maximumFractionDigits: 1) }% loss, direct
# As above, over a relay.
voice-path-relay = { $ms } ms, { NUMBER($loss, minimumFractionDigits: 1, maximumFractionDigits: 1) }% loss, through a relay
# As above, while the round trip runs well past what it usually is ($usual, in milliseconds).
voice-path-usual-direct = { $ms } ms (usually { $usual } ms), { NUMBER($loss, minimumFractionDigits: 1, maximumFractionDigits: 1) }% loss, direct
# As above, over a relay.
voice-path-usual-relay = { $ms } ms (usually { $usual } ms), { NUMBER($loss, minimumFractionDigits: 1, maximumFractionDigits: 1) }% loss, through a relay
# A badge on a member whose link to you is losing packets. $percent is a whole number.
voice-link-loss = { $percent }% loss
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
voice-unstable = Your connection is unstable
voice-unstable-tip = Most of your links are slow or losing packets
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
voice-here-you = You are in a voice call here
voice-here-someone = Someone is in a voice call
