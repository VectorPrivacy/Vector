### Sending this account to another device

## Approving the transfer with your PIN or password

transfer-approve-title = Approve the Transfer
transfer-approve-pin = Enter your PIN to send this account to the new device.
transfer-approve-password = Enter your password to send this account to the new device.

## The card's title at each step

transfer-title-match = Check the number
transfer-title-approve = Sign in on a new device?
transfer-title-finishing = Signing in
transfer-title-sent = Sent
transfer-title-done = All set
transfer-title-stopped = Transfer stopped
# On the device that has the account. Matches the button in Settings.
transfer-title-send = Sign in on another device
# On the device being signed in. Matches the option on the login screen.
transfer-title-receive = Sign in with another device

## Scanning the other device's code

transfer-pick-lead = On your signed-in device, open Settings and choose Sign in on Another Device. Then scan the QR code it shows.
transfer-scan = Scan QR Code
transfer-text-instead = Use a Text Code Instead
transfer-scan-other-instead = Scan the other device instead
transfer-scan-instead = Scan instead

## Showing a code

transfer-unnamed = Unnamed account
transfer-code-lead-send = On the new device, tap Login, then Sign in with another device, and scan this or type the code.
transfer-code-lead-receive = On your signed-in device, open Settings and choose Sign in on another device, then scan this or type the code.
transfer-copy = Copy
transfer-code-copied = Copied Code
# $clock is minutes and seconds left, like "4:59".
transfer-expires = Expires in { $clock }
transfer-expired = Expired
transfer-new-code = Get a New Code
transfer-type-instead = Type the other device's code instead

## Typing a code

transfer-enter-lead = Type the code shown on your other device. It's a number and three words.
transfer-connect = Connect
transfer-connecting = Connecting…
transfer-show-instead = Show a code on this device instead

## Matching the number

transfer-match-lead = On your other device, type this number to approve.
# Above the name and picture of the account being signed in to.
transfer-signing-in-as = Signing in as
# <b> is shown in white.
transfer-approve-warning = You're giving full, permanent control of this account to another device. <b>Only continue if you're setting up your own device right now.</b> Vector never asks for this to verify, unlock or join anything.
# Shown in capitals.
transfer-number-label = Number shown on the new device
transfer-approve = Approve
transfer-deny = Deny
# $count is how many tries are left.
transfer-wrong-number = { $count ->
    [one] That isn't the number on the new device. { $count } try left.
   *[other] That isn't the number on the new device. { $count } tries left.
}

## Sending and finishing

transfer-sending = Sending your account…
transfer-sent-unconfirmed = Your account is on its way. Check the new device to finish signing in.
transfer-sent = Your new device is signing in. Your chats and communities will sync there on their own.
transfer-done = Done
transfer-signing-in = Signing in…
transfer-try-again = Try Again

## When it stops

transfer-stopped-early = The transfer stopped before it began
transfer-ended = The transfer has ended
transfer-stopped = The transfer stopped.
transfer-arrival-expired = The account that arrived has expired. Start again.
# $error is why.
transfer-finish-failed = Couldn't finish signing in: { $error }

## Why an account transfer was refused

transfer-not-waiting = The transfer is no longer waiting for approval
transfer-signer-account = This account's key lives in your signer app, so there's nothing to send from here. Sign in with your signer on the other device.
transfer-sign-in-first = Sign in first
transfer-account-changed = The account changed, so nothing was sent
transfer-key-mismatch = This device's stored key doesn't match the account, so nothing was sent
