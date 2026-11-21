### The Moderation console: its members, the raid warning, and the removal and ban actions.

## Header and tabs

# Shown over the community's name, in capitals
moderation-title = Moderation
# Stands in for a community with no name
moderation-unnamed = Community
# $epoch is the community's key generation, a number.
moderation-epoch = Epoch { $epoch }
moderation-close = Close
moderation-tab-members = Members
moderation-tab-policies = Policies
moderation-search = Search members...

## Lengths of time, written short: 5s, 3m, 2h, 4d, 1.5y

moderation-ago-unknown = unknown
# $count is a number of seconds.
moderation-ago-seconds = { $count }s
# $count is a number of minutes.
moderation-ago-minutes = { $count }m
# $count is a number of hours.
moderation-ago-hours = { $count }h
# $count is a number of days.
moderation-ago-days = { $count }d
# $count is a number of years, with one decimal place.
moderation-ago-years = { NUMBER($count, minimumFractionDigits: 1, maximumFractionDigits: 1) }y

## The raid warning

# $count is how many accounts look like raiders.
moderation-raid = { $count ->
    [one] Raid: { $count } account flagged.
   *[other] Raid: { $count } accounts flagged.
}
# $count is how many accounts posted the same thing, $sample the start of what they posted.
moderation-raid-cohort = { $count } posted “{ $sample }”.
# $count is how many joined, $span how close together, such as 3m.
moderation-raid-burst = { $count } joined within { $span } of each other.
moderation-raid-unticked = They start unticked, so they are the ones being removed.
moderation-publishing = Publishing. Leave this open until it finishes.

## Counts across the top, shown in capitals

moderation-stat-members = Members
moderation-stat-trusted = Trusted
moderation-stat-staff = Staff
moderation-stat-flagged = Flagged

## The member list

moderation-filter-all = Everyone
moderation-filter-removing = Removing
moderation-filter-staying = Staying
moderation-kept-note = Everyone is kept by default. Untick someone to cut them off at the next rotation.
moderation-no-match = No members match.
# A member's protection, when the reason isn't given
moderation-protected = Protected
# Badges beside a member, shown in capitals
moderation-badge-owner = Owner
moderation-badge-you = You
moderation-badge-staff = Staff
moderation-badge-regular = Regular
moderation-badge-check = Check
# How long a member has been here. $age is short, such as 3d. The <b> part is emphasised.
moderation-meta-here = here <b>{ $age }</b>
moderation-meta-age-unknown = age unknown
# How many messages a member has sent. The <b> part is emphasised.
moderation-meta-messages = { $count ->
    [one] <b>{ $count }</b> msg
   *[other] <b>{ $count }</b> msgs
}
# How many of a member's messages differ from each other. The <b> part is emphasised.
moderation-meta-distinct = <b>{ $count }</b> distinct
# $label is the name of the invite link they joined through.
moderation-meta-via = via { $label }

## The footer

# $count is how many members stay. The <b> part is emphasised.
moderation-staying = <b>{ $count }</b> staying
# $count is how many members are being removed. The <b> part is emphasised.
moderation-removing = <b>{ $count }</b> removing
# How full the banlist is. $count is how many are banned, $max the most it holds.
moderation-banlist = banlist { $count }/{ $max }
# $count is how many live invite links there are.
moderation-revoke = { $count ->
    [one] Revoke { $count } invite
   *[other] Revoke { $count } invites
}
moderation-no-invites = No invite links
# $count is how many members are being removed.
moderation-remove-rotate = Remove { $count } & rotate
moderation-rotate = Rotate keys
# $count is how many members are being banned.
moderation-ban-rotate-count = Ban { $count } & rotate
moderation-ban-rotate = Ban & rotate
# $max is the most the banlist holds, $room how many places are left.
moderation-banlist-full = { $room ->
    [one] The banlist holds { $max }; only { $room } slot is free. Rotate instead: it has no ceiling.
   *[other] The banlist holds { $max }; only { $room } slots are free. Rotate instead: it has no ceiling.
}

## Confirming an action

moderation-revoke-title = Revoke every invite link
# $count is how many public invite links there are.
moderation-revoke-text = { $count ->
    [one] Retire the public invite link? Anyone holding it can no longer join. Existing members are unaffected.
   *[other] Retire all { $count } public invite links? Anyone holding one can no longer join. Existing members are unaffected.
}
moderation-remove-title = Remove members and rotate
# $count is how many members are removed, $kept how many remain.
moderation-remove-text = { $count ->
    [one] Remove { $count } member from the community, then mint a new epoch only the { $kept } remaining can follow. They are dropped from everyone's member list and lose access, without being banned.
   *[other] Remove { $count } members from the community, then mint a new epoch only the { $kept } remaining can follow. They are dropped from everyone's member list and lose access, without being banned.
}
# $count is how many members are removed.
moderation-remove-slow = This publishes one removal per member, so { $count } will take a while.
moderation-rotate-title = Rotate keys
moderation-rotate-text = Mint a new epoch for everyone currently in the community. Use this when an invite link leaked but the members are all real.
# $count is how many invite links still work.
moderation-links-live = { $count ->
    [one] 1 invite link is still live. Anyone holding it can rejoin straight after this. Revoke it first.
   *[other] { $count } invite links are still live. Anyone holding one can rejoin straight after this. Revoke them first.
}
moderation-ban-title = Ban and rotate
# $count is how many members are banned.
moderation-ban-text = { $count ->
    [one] Ban { $count } member and rotate the keys around them. They're added to the banlist, stripped of any role, and cannot rejoin until unbanned.
   *[other] Ban { $count } members and rotate the keys around them. They're added to the banlist, stripped of any role, and cannot rejoin until unbanned.
}

## Progress and results

moderation-revoking = Revoking invite links…
# $count is how many links were revoked.
moderation-revoked = { $count ->
    [one] Revoked { $count } invite link
   *[other] Revoked { $count } invite links
}
# $count is how many were revoked, $failed how many couldn't be.
moderation-revoked-some = Revoked { $count }, { $failed } failed
moderation-revoke-failed = Couldn't revoke
# $count is how many members are being banned.
moderation-banning = Banning { $count }…
# $done is how many have been removed so far, $total how many in all.
moderation-removing-progress = Removing { $done }/{ $total }
# $percent is how far it has got.
moderation-removing-percent = { $percent }% — leave this open until it finishes.
moderation-rotating = Rotating keys…
# $count is how many members were banned.
moderation-banned = Banned { $count } and rotated.
# $count is how many members were removed.
moderation-removed = Removed { $count } and rotated.
# $count is how many members were removed, $refused how many refused it.
moderation-removed-refused = Removed { $count } and rotated. { $refused } refused.
moderation-rotated = Keys rotated.
moderation-ban-failed = Couldn't ban
moderation-ban-unrotated = Banned, but the keys did NOT rotate
# $count is how many were banned.
moderation-ban-unrotated-text = { $count ->
    [one] The { $count } account is banned and cannot post. It still holds the current key and can READ until a rotation lands. Use Rotate keys to finish.
   *[other] The { $count } accounts are banned and cannot post. They still hold the current key and can READ until a rotation lands. Use Rotate keys to finish.
}
moderation-remove-failed = Couldn't complete the removal
moderation-no-data = No moderation data.
