### Vector's British English, the only language built into the app. Every other language
### mirrors these files. A message whose English meaning changes gets a new id.

## Date dividers in the message timeline

day-today = Today
day-yesterday = Yesterday

## Typing

# The chat list's preview line while the other person in a DM is typing.
typing-dm = Typing...

# The chat list's preview line while people in a group are typing.
# $count is how many are typing; $first, $second and $third are the first three names
# (empty when there are fewer).
typing-group = { $count ->
    [1] { $first } is typing...
    [2] { $first } and { $second } are typing...
    [3] { $first }, { $second }, and { $third } are typing...
   *[other] Several people are typing...
}

# The strip above the composer naming who is typing in the open chat. <b> marks a name.
# $count is how many are typing; $first, $second and $third are the first three names
# (empty when there are fewer).
typing-strip = { $count ->
    [1] <b>{ $first }</b> is typing
    [2] <b>{ $first }</b> and <b>{ $second }</b> are typing
    [3] <b>{ $first }</b>, <b>{ $second }</b> and <b>{ $third }</b> are typing
   *[other] Several people are typing
}
