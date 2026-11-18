### The / command picker, and the commands built into Vector.

## What each built-in command does. $colour is "colour" or "color", as the reader spells it,
## and $colours its plural; another language can use its own word instead.

# $colour is "colour" or "color".
command-text-arg = The message to { $colour }
command-message-arg = What to say before it
# $colours is "colours" or "colors".
command-rainbow = Send your message in rainbow { $colours }
# $colour is "colour" or "color".
command-gradient = Fade your message from one { $colour } to another
# $colour is "colour" or "color".
command-gradient-from = The { $colour } it starts in: a name or #hex
# $colour is "colour" or "color".
command-gradient-to = The { $colour } it ends in: a name or #hex
# $colour is "colour" or "color".
command-colour = Send your message in one { $colour }
# $colour is "colour" or "color".
command-colour-arg = Its { $colour }: a name or #hex
command-time = A time everyone sees in their own zone
# The examples are typed in English, which is what Vector reads, so keep them as they are.
command-time-arg = Like “friday 8pm” or “in 2 hours”
# $face is a text face, like ¯\_(ツ)_/¯.
command-face = Add { $face } to your message
command-coinflip = Flip a coin: heads or tails
# Sent into the chat. ** makes the result bold.
command-coinflip-heads = 🪙 Coin flip: **Heads**
# Sent into the chat. ** makes the result bold.
command-coinflip-tails = 🪙 Coin flip: **Tails**

## When a command's arguments don't fit. $name is the argument's name.

# $name is the argument.
command-arg-too-long = "{ $name }" is too long (max 1024 characters)
# $name is the argument.
command-arg-whole-number = "{ $name }" must be a whole number
# $name is the argument.
command-arg-number = "{ $name }" must be a number
# $name is the argument.
command-arg-bool = "{ $name }" must be true or false
# $name is the argument.
command-arg-user = "{ $name }" must be a user (npub)
# $name is the argument and $choices lists what it can be.
command-arg-choice = "{ $name }" must be one of: { $choices }
# $name is the argument, and $colour is "colour" or "color".
command-arg-colour = "{ $name }" must be a { $colour } name or #hex
# $name is the argument.
command-arg-missing = Missing required argument "{ $name }"
# $command is the command's name.
command-unclosed-quote = Unclosed quote in /{ $command }
# $command is the command's name and $error what's wrong.
command-error = /{ $command }: { $error }

## The picker

# $count is how many bots are loading.
command-loading-bots = { $count ->
    [one] Loading { $count } bot…
   *[other] Loading { $count } bots…
}
command-finding-bots = Looking for bots…
command-recent = Recently Used
command-checking-updates = Checking for Updates
# The strip above the composer while a command's form is open. $command is the command with
# its slash ("/roll"), drawn in the accent colour; <bot> marks the bot, drawn with its picture.
command-using = Using <b>{ $command }</b> with <bot>{ $bot }</bot>
