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

## The invite screen

invite-public-tip = Anyone with a link can join
invite-private-tip = Invite-only — no public links
# $count is how many invite links other members made.
invite-view-others = { $count ->
    [one] View { $count } other invite
   *[other] View { $count } other invites
}
# $name made $count invite links that still work.
invite-member-links = { $count ->
    [one] { $name } has { $count } active invite link
   *[other] { $name } has { $count } active invite links
}
invite-none-yet = No active links yet. Create one to start inviting.
invite-none-own = You have no links of your own yet. Create one to start inviting.
invite-links-title = Invite Links
invite-links-text = Anyone with a link can join. Revoke every link to go private again.
# A link with no label. $tail is the end of its code.
invite-link-unlabelled = Invite · { $tail }
# $count is how many people joined through the link.
invite-link-joins = { $count ->
    [one] { $count } member joined via this link
   *[other] { $count } members joined via this link
}
invite-copy-link = Copy link
invite-revoke-link = Revoke link
invite-creating-short = Creating…
invite-create-link = Create invite link
# $name is the community's name.
invite-title = Invite to { $name }
invite-lead = Bring people into your community.
invite-direct-title = Direct Invites
invite-direct-text = Pick contacts to invite, or paste an npub to add someone new.
# $count is how many contacts are picked.
invite-send = Invite { $count }
invite-done = Done
