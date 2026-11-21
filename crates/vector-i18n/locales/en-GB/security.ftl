### Local Encryption (PIN or password), and showing your private keys.

## The two ways to protect local data. PIN and Password are tile titles; the descriptions sit
## under them.

security-pin = PIN
security-password = Password
security-pin-desc = A 6-digit code. Quick and convenient.
security-password-desc = A text password. More secure, but slower to enter.
security-continue = Continue

## Checking the PIN or password you entered. Each comes in a PIN and a password form.

security-checking-pin = Checking your PIN…
security-checking-password = Checking your password…
security-incorrect-pin = Incorrect PIN, try again.
security-incorrect-password = Incorrect password, try again.
# $count is how many minutes until another try is allowed.
security-locked-minutes = Too many wrong tries. Try again in { $count } min.
# $count is how many seconds until another try is allowed.
security-locked-seconds = Too many wrong tries. Try again in { $count } s.
security-verify-failed = Could not verify

## Choosing a new PIN or password

security-pin-rules = Enter a 6-digit PIN.
security-password-rules = Enter a password (4+ characters).
security-password-short = Too short. Use at least 4 characters.
security-create-pin = Create PIN
security-create-password = Create Password
security-confirm-pin = Confirm PIN
security-confirm-password = Confirm Password
security-reenter-pin = Re-enter your PIN.
security-reenter-password = Re-enter your password.
security-mismatch-pin = PINs didn't match. Try again.
security-mismatch-password = Passwords didn't match. Try again.

## Turning encryption on

security-enable-title = Set Up Encryption
security-enable-text = Choose how to protect your local data. There is no recovery if you forget!
security-enabling = Enabling Encryption
security-enabled = Encryption enabled
security-enable-failed = Encryption Failed
# $error is why.
security-enable-failed-text = Failed to enable encryption: { $error }

## Turning encryption off

security-disable-title = Disable Encryption?
# <b> is bold, <br> a line break.
security-disable-text = This will decrypt all your local data and remove PIN protection.<br><br><b>Your messages and keys will be stored in plain text.</b><br><br>This is useful for faster app startup on personal devices, but reduces security if your device is lost or stolen.
security-disable-enter-pin = Enter PIN
security-disable-enter-password = Enter Password
security-disable-lead-pin = Enter your PIN to turn off Local Encryption.
security-disable-lead-password = Enter your password to turn off Local Encryption.
security-disabling = Disabling Encryption
security-disabled = Encryption disabled
security-disable-failed = Decryption Failed
# $error is why.
security-disable-failed-text = Failed to disable encryption: { $error }

## Changing the PIN or password

security-current-pin = Enter Current PIN
security-current-password = Enter Current Password
security-current-lead-pin = Please enter your current PIN to continue.
security-current-lead-password = Please enter your current password to continue.
security-change-pin = Change PIN
security-change-password = Change Password
security-change-text = Choose a new security type and credential.
security-changing = Changing Credential
security-changed = Unlock method updated
security-change-failed = Re-keying Failed
# $error is why.
security-change-failed-text = Failed to change credential: { $error }

## While data is being encrypted or decrypted

security-preparing = Preparing...
# $done is how many messages are finished and $total how many there are.
security-phase-decrypting = Decrypting messages... { $done } / { $total }
# $done is how many messages are finished and $total how many there are.
security-phase-encrypting = Encrypting messages... { $done } / { $total }
# $done is how many messages are finished and $total how many there are.
security-phase-rekeying = Re-encrypting messages... { $done } / { $total }
security-phase-finalizing = Finalizing...
security-dont-close = Do not close the app during this process

## Showing your private keys

security-keys-ask = Show Private Keys?
security-keys-title = Your Keys
security-keys-enter = Show Private Keys
security-keys-lead-pin = Enter your PIN to show your keys.
security-keys-lead-password = Enter your password to show your keys.
security-keys-transfer = Sign in on Another Device
# <b> is shown in white. $button is the name of the Sign in on Another Device button.
security-keys-transfer-hint = To use Vector on another device, use <b>{ $button }</b> instead. It's quicker, and your keys never leave this device in a form anyone else can read.
security-keys-warning = Only show your keys to back them up offline or to use them in another Nostr app. Anyone who sees them can read your messages and act as you.
security-keys-proceed = Show Keys Anyway
# <b> is shown in white.
security-keys-lead = Your keys are your account. Anyone who has them can read your messages and speak as you. <b>Write them down offline and never share them.</b>
security-keys-seed = Seed Phrase
security-keys-nsec = Private Key
security-keys-copy = Copy
security-keys-reveal = Tap to reveal
security-keys-done = Done
security-keys-moving = Moving to another device? Sign in there with a code instead
security-export-failed = Export Failed

## Why an encryption change or key export was refused

security-migration-running = A migration is already in progress
security-wait-for-change = Wait for the encryption change to finish
security-not-enabled = Local Encryption is not enabled
security-already-enabled = Encryption is already enabled
security-account-changed = Account changed, nothing was modified
security-export-external = This is an external signer account. Your identity key lives on your signer app, never on this device, so there's nothing to export here.
security-export-account-changed = The account changed, so nothing was exported

## Changing how the account is encrypted

security-rekey-starting = Starting...
security-rekey-keep-open = Do not close the app during this process

## The phone's fingerprint or screen lock as the only credential

security-biometric-offer-title = Protect with Biometrics?
# <b> is bold, <br> a line break.
security-biometric-offer-text = Unlock Vector with your fingerprint, face, or device PIN. No Vector PIN to remember.<br><br>Choose Cancel to set a PIN or Password instead.
security-use-biometrics = Use Biometrics
security-biometric-warning-title = Before you continue
# <b> is bold, <br> a line break.
security-biometric-warning-text = Vector will be protected by your device's screen lock: fingerprint, face, or device PIN.<br><br><b>Do not remove your phone's screen lock without first disabling Local Encryption in Vector</b>, or this device loses access to its encrypted data and you will need to sign in with your keys again.<br><br>Changing your PIN or adding fingerprints is always safe.
security-biometric-rejected = Biometric hardware rejected the key, try again
security-biometric-failed = Could not enable biometrics
security-switch-credential-title = Switch to PIN or Password
security-switch-credential-text = Choose the credential that will unlock Vector from now on. There is no recovery if you forget it!
security-switch-biometric-lead-pin = Enter your current PIN to switch to biometric unlock.
security-switch-biometric-lead-password = Enter your current password to switch to biometric unlock.
security-now-biometric = Now unlocking with biometrics
security-switch-failed = Could not switch
