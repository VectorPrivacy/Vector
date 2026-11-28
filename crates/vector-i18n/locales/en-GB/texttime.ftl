### Times written into messages, shown in each reader's own zone.

## How far away a time is

time-relative-now = now
# $count is how many seconds from now.
time-in-seconds = { $count ->
    [one] in { $count } second
   *[other] in { $count } seconds
}
# $count is how many minutes from now.
time-in-minutes = { $count ->
    [one] in { $count } minute
   *[other] in { $count } minutes
}
# $count is how many hours from now.
time-in-hours = { $count ->
    [one] in { $count } hour
   *[other] in { $count } hours
}
# $count is how many days from now.
time-in-days = { $count ->
    [one] in { $count } day
   *[other] in { $count } days
}
# $count is how many months from now.
time-in-months = { $count ->
    [one] in { $count } month
   *[other] in { $count } months
}
# $count is how many years from now.
time-in-years = { $count ->
    [one] in { $count } year
   *[other] in { $count } years
}
# $count is how many seconds ago.
time-ago-seconds = { $count ->
    [one] { $count } second ago
   *[other] { $count } seconds ago
}
# $count is how many minutes ago.
time-ago-minutes = { $count ->
    [one] { $count } minute ago
   *[other] { $count } minutes ago
}
# $count is how many hours ago.
time-ago-hours = { $count ->
    [one] { $count } hour ago
   *[other] { $count } hours ago
}
# $count is how many days ago.
time-ago-days = { $count ->
    [one] { $count } day ago
   *[other] { $count } days ago
}
# $count is how many months ago.
time-ago-months = { $count ->
    [one] { $count } month ago
   *[other] { $count } months ago
}
# $count is how many years ago.
time-ago-years = { $count ->
    [one] { $count } year ago
   *[other] { $count } years ago
}

## A full date and time

# $date is a date, like "Monday, March 3, 2025", and $time a time of day.
time-date-at = { $date } at { $time }
# The same with the time zone, like "GMT+1", after the time.
time-date-at-zone = { $date } at { $time } { $zone }

## The time picker in the composer

time-picker-title = Time
# The examples are typed in English, which is what Vector reads, so keep them as they are.
time-picker-hint = Try “tomorrow 5pm”, “next monday”, “dec 25” or “in 2 hours”
time-style-countdown = Countdown
time-style-since = Time since
time-style-time = Time
time-style-date = Date
time-style-date-time = Date and time
# $when is the full date and time.
time-suggest-countdown = Make it a countdown: { $when }
# $when is the full date and time.
time-suggest-local = Show in everyone’s time: { $when }
