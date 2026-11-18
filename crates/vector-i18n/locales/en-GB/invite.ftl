### Inviting people to a community: its links, and making it private.

## Naming a new invite link

invite-label-title = Label this link
invite-label-text = Optional. A label lets you see which link people join through (e.g. "Reddit", "Twitter"). Leave blank to skip.
invite-label-create = Create link
invite-label-placeholder = Label (optional)

## Progress

invite-creating = Creating link...
invite-revoking = Revoking…
invite-privatizing = Making community private
invite-privatized = Community is now private
invite-privatize-failed = Couldn't make private

## Making a community public or private through its links

invite-public-title = Make community public?
# <b> is bold.
invite-public-text = Creating an invite link makes this community <b>public</b>: anyone with the link can join. You can make it private again later by revoking every link.
invite-public-confirm = Make public
invite-private-title = Make community private?
# <b> is bold.
invite-private-text = Revoking the last invite link makes this community <b>private</b> again. This can take a few seconds.
invite-private-confirm = Make private

## Inviting people directly

# $count is how many people are being invited.
invite-sending = { $count ->
    [one] Inviting { $count } person…
   *[other] Inviting { $count } people…
}
# $count is how many people were invited.
invite-sent = { $count ->
    [one] Invited { $count } person!
   *[other] Invited { $count } people!
}
# $ok is how many invites went out and $failed how many didn't.
invite-partial = Invited { $ok }, { $failed } failed.

## A community link's visibility tag. Shown in capitals.

invite-public = Public
invite-private = Private
