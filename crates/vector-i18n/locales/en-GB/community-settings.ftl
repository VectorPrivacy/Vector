### A community's settings: its pages and their sections.

## Pages

community-settings-page-overview = Overview
community-settings-page-relays = Relays
community-settings-page-roles = Roles
community-settings-page-bans = Bans
community-settings-page-audit = Audit Log

## Sections inside the pages. Each section's .keys are the words a search finds it by, separated by spaces.

community-settings-section-identity = Icon & Name
    .keys = icon avatar logo picture image name title rename
community-settings-section-banner = Banner
    .keys = banner cover header art background picture image
community-settings-section-description = Description
    .keys = description about bio summary
community-settings-section-system-messages = System Messages
    .keys = system messages join leave joined left welcome channel
community-settings-section-landing-channel = Landing Channel
    .keys = landing channel first start new members rules welcome verify captcha onboarding
community-settings-section-relay-list = Hosting Relays
    .keys = relays servers hosting network nostr
community-settings-section-voice-relays = Voice Relays
    .keys = voice relays iroh calls audio
community-settings-section-ban-list = Banned Members
    .keys = bans banned unban blocked removed
community-settings-section-audit-list = Audit Log
    .keys = audit log history changes actions who when record

## Checking a relay address as it's added

community-settings-relay-invalid = That isn't a relay address.
community-settings-relay-scheme = Use wss://, or ws:// for an .onion or .i2p address.
community-settings-relay-listed = That relay is already listed.
community-settings-voice-relay-scheme = A voice relay is an https:// address.
# $count is the most voice relays a community can list.
community-settings-voice-relays-max = { $count ->
    [one] At most { $count } voice relay.
   *[other] At most { $count } voice relays.
}

## Overview

# A tag on an icon or banner picked but not saved yet
community-settings-new = New
community-settings-add-banner = Add a banner
community-settings-no-banner = No banner
community-settings-change-icon = Change icon
community-settings-change-banner = Change banner
community-settings-remove = Remove
community-settings-remove-banner = Remove Banner
community-settings-name = Community Name
community-settings-icon-hint = The icon shows on the rail and beside every invite. Square images crop best.
community-settings-description-hint = What this community is about
community-settings-system-messages = Join and leave messages
community-settings-system-messages-hint = In a private channel, only the members who can read it see them.
community-settings-landing = Where new members start
community-settings-landing-hint = It opens first after joining, until they pick another channel.
community-settings-landing-first = First channel they can read
community-settings-system-messages-off = Off

## Relays

community-settings-auth-tip = What AUTH means
community-settings-add = Add
community-settings-no-relays = No relays recorded for this community.
community-settings-iroh-relays = Using iroh's public relays, run by n0.
community-settings-learn-more = Learn more
community-settings-relays-lede = Every message, channel and member list in this community lives on these relays, end-to-end encrypted.
community-settings-relays-hint = A new relay is checked and given this community's whole history before anyone moves to it, and the old relays tell members where everyone went.
community-settings-voice-relays-lede = Voice channels connect through these relays, end-to-end encrypted. Each relay sees where callers connect from.

## Bans

community-settings-no-matches = No matches.
community-settings-no-bans = Nobody is banned from this community.
# $count is how many banned members the list shows.
community-settings-select-all = Select all ({ $count })
# $count is how many banned members the list shows.
community-settings-deselect-all = Deselect all ({ $count })
# $count is how many banned members match the search.
community-settings-select-matches = Select matches ({ $count })
# $count is how many banned members match the search.
community-settings-deselect-matches = Deselect matches ({ $count })
community-settings-bans-lede = A banned member is silenced everywhere in this community and can't rejoin until they're unbanned.
# How full the ban list is. $count is how many are banned, $max the most it holds.
community-settings-bans-count = { $count } of { $max }
community-settings-unban-all = Unban All
# $count is how many selected members the button unbans.
community-settings-unban = Unban { $count }
# $count is how many members are being unbanned.
community-settings-unbanning = Unbanning { $count }...
# $count is how many banned members are selected.
community-settings-selected = { $count ->
    [one] { $count } member selected
   *[other] { $count } members selected
}
community-settings-clear = Clear

## Audit Log

community-settings-audit-reading = Reading this community's history…
community-settings-audit-empty = Nothing has happened here yet.
community-settings-show-more = Show more
community-settings-audit-compacted = Some history from before a key rotation this device didn't live through can't be read back.
community-settings-sync-report = Sync Report
community-settings-refresh = Refresh
community-settings-audit-lede = Every change to this community, signed by whoever made it.

## The save bar

# $percent is how much of an upload has gone.
community-settings-saving-progress = Saving { $percent }%
community-settings-saving = Saving...
community-settings-save-before-closing = Save or reset your changes before closing
community-settings-unsaved = You have unsaved changes
community-settings-reset = Reset
community-settings-save = Save Changes

# The title over the pages when the community has no name
community-settings-unnamed = Community

## Sync Report, an Advanced-mode diagnostic. Its findings stay in English, as in the copied report.

community-settings-report-reading = Reading the control plane…
community-settings-report-flagged = Worth a look
community-settings-report-full = Full report
community-settings-report-lede = How this device reads the community. Versions and short IDs only, safe to share.
community-settings-report-close = Close
community-settings-report-copy = Copy Report

## Leaving or deleting a community

community-settings-not-deleted = Not Deleted
community-settings-not-deleted-text = The name did not match, so nothing was changed.
community-settings-delete-failed = Failed to Delete
community-settings-leave-failed = Failed to Leave
