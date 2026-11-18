### Desktop notifications.

# The title when the sender's name isn't known yet.
notify-new-message = New Message
# A community poster whose name isn't known yet.
notify-someone = Someone

## The notification menu of a chat, channel or community

notify-settings = Notification Settings
notify-unavailable = Notification settings unavailable
notify-level-all = All messages
notify-level-mentions = Mentions
notify-level-nothing = Nothing
# A channel following its community. $level is one of the three levels above.
notify-use-default = Use Community Default ({ $level })
# Whether @everyone pings come through.
notify-everyone-allow = Allow
notify-everyone-suppress = Suppress
# Beside Allow when the app turns @everyone off everywhere.
notify-everyone-off = off in App Settings
notify-mute = Mute
notify-muted = Muted
notify-unmute = Unmute
notify-muted-by-community = Muted by the community
# Beside Muted. $time is when it ends, like "18:30" or "Tue 18:30".
notify-muted-until = until { $time }
# $count is how many minutes.
notify-mute-minutes = { $count ->
    [one] For { $count } minute
   *[other] For { $count } minutes
}
# $count is how many hours.
notify-mute-hours = { $count ->
    [one] For { $count } hour
   *[other] For { $count } hours
}
notify-mute-forever = Until I turn it back on
