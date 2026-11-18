### Words and rows shared by many screens.

## Popup buttons

dialog-okay = Okay
dialog-confirm = Confirm
dialog-cancel = Cancel

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

time-seconds = { $count ->
    [one] { $count } second
   *[other] { $count } seconds
}
time-minutes = { $count ->
    [one] { $count } minute
   *[other] { $count } minutes
}
time-hours = { $count ->
    [one] { $count } hour
   *[other] { $count } hours
}
time-days = { $count ->
    [one] { $count } day
   *[other] { $count } days
}
time-weeks = { $count ->
    [one] { $count } week
   *[other] { $count } weeks
}

## The Main Menu toggle on anything that can be pinned there

main-menu-pin = Pin to Main Menu
main-menu-unpin = Unpin

## Numbers

# A percentage, such as a volume or a share of lost audio. $value is the number.
percent = { $value }%

## Joining names

# Two choices, either of which will do. $first and $second are names, such as Tor and I2P.
either = { $first } or { $second }
