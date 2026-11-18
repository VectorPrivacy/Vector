### Words and rows shared by many screens.

## Popup buttons

dialog-okay = Okay
dialog-confirm = Confirm
dialog-cancel = Cancel
dialog-close = Close
dialog-open = Open
dialog-rename = Rename
dialog-save = Save

## The window's title bar

window-minimize = Minimize
window-restore = Restore
window-maximize = Maximize
window-close = Close
# Opens the online docs.
help = Help

## Shown where an action's answer never came back, because the app's background half stopped

backend-stopped = Backend stopped

## Toasts once something is on the clipboard

copied = Copied
copied-link = Copied Link
copied-clipboard = Copied to Clipboard
copied-app-link = Copied App Link
copy-link = Copy Link
# The answer when a save never came back.
save-failed = Save failed

## A download's progress

# $done and $total are megabytes.
progress-mb = { NUMBER($done, minimumFractionDigits: 1, maximumFractionDigits: 1) }/{ NUMBER($total, minimumFractionDigits: 1, maximumFractionDigits: 1) } MB

## Shown where an action was dropped before it finished

cancelled = Cancelled

## The placeholder in a search field

search = Search

## Copying an id, in Advanced mode: the menu row, and .done is the toast once it's copied

copy-id-message = Copy Message ID
    .done = Copied Message ID
copy-id-user = Copy User ID
    .done = Copied User ID
copy-id-role = Copy Role ID
    .done = Copied Role ID
copy-id-community = Copy Community ID
    .done = Copied Community ID
copy-id-channel = Copy Channel ID
    .done = Copied Channel ID

## Lengths of time, for choices like a timeout's

# $count is how many seconds.
time-seconds = { $count ->
    [one] { $count } second
   *[other] { $count } seconds
}
# $count is how many minutes.
time-minutes = { $count ->
    [one] { $count } minute
   *[other] { $count } minutes
}
# $count is how many hours.
time-hours = { $count ->
    [one] { $count } hour
   *[other] { $count } hours
}
# $count is how many days.
time-days = { $count ->
    [one] { $count } day
   *[other] { $count } days
}
# $count is how many weeks.
time-weeks = { $count ->
    [one] { $count } week
   *[other] { $count } weeks
}

## A countdown's short form, over a minute; under one it's a clock like 3:05

# $days and $hours are whole numbers, like "2d 5h".
countdown-days = { $days }d { $hours }h
# $hours and $minutes are whole numbers, like "3h 20m".
countdown-hours = { $hours }h { $minutes }m

## The Main Menu toggle on anything that can be pinned there

main-menu-pin = Pin to Main Menu
main-menu-unpin = Unpin

## Numbers

# A percentage, such as a volume or a share of lost audio. $value is the number.
percent = { $value }%

## Joining names

# Two choices, either of which will do. $first and $second are names, such as Tor and I2P.
either = { $first } or { $second }

## Stand-in names

# A community whose name hasn't arrived.
community-unnamed = Community
