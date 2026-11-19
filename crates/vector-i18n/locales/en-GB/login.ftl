## The welcome screen: signing in, creating an account, and the network choice before either.
## Tor and I2P are names.

login-net = Network
login-net-about = About these networks
# $network is the network's name, like Tor or I2P.
login-net-couldnt-connect = { $network } couldn't connect.
# $network is the network's name, like Tor or I2P.
login-net-not-yet = { $network } isn't connected yet. Try again, or pick another network.
# $network is the network's name, like Tor or I2P.
login-net-connecting = { $network } connecting…
login-net-keep-open = Do not close app.
login-net-change-port = Change Port
login-net-sam-port = SAM port
login-net-sam-only-if = Only if your router asks for one.
login-net-sam-both = Enter both a username and a password.
login-net-sam-chars = Use letters, numbers and symbols, without spaces or quotes.
login-net-use = Use

## Signing in

login-method-qr = QR Code
login-method-key = Private Key
login-method-signer = Remote Signer
login-sign-in-with = Sign in with
login-sign-in-with-about = About these ways to sign in
login-key-placeholder = Enter nsec or Seed Phrase...
login-password-placeholder = Enter Password...
login-bunker-placeholder = bunker:// from Amber, etc.
login-select-profile = Select Profile
login-create-account = Create Account
login-login = Login
login-enter-key = Enter Your Private Key
login-with-qr = Login with QR Code
login-another-device = Sign in with Another Device
# The words inside <back> are the link that goes back.
login-back-changed-mind = Changed your mind? <back>Go Back</back>
# The words inside <back> are the link that goes back.
login-back-change-accounts = Want to change accounts? <back>Back to Login</back>
login-continue = Continue
login-show-password = Show password
login-hide-password = Hide password

## Encrypting the keys on this device: a PIN or a password, or none

login-skip-encryption = Skip Encryption
login-create-password = Create Password
login-create-pin = Create PIN
login-recommended = *Recommended Option
login-recommended-options = *Recommended Options
login-setting-up = Setting up your account...
login-decrypting = Decrypting your keys...
login-encrypting = Encrypting your keys...
login-strengthening = Strengthening your encryption…
# $percent is a whole number from 0 to 100.
login-strengthening-at = Strengthening your encryption… { $percent }%
login-enter-pin = Enter Your Decryption Pin
login-create-your-pin = Create Your Pin
login-reenter-pin = Re-enter Your Pin
login-enter-password = Enter Your Password
login-create-your-password = Create Your Password
login-reenter-password = Re-enter Your Password
login-password-short = Password must be at least 4 characters
login-pin-mismatch = Pin doesn't match, re-try
login-password-mismatch = Passwords don't match, re-try
login-pin-incorrect = Incorrect pin, try again
login-password-incorrect = Incorrect password, try again
login-wrong-pin = Oops! You entered the wrong pin!
login-wrong-password = Oops! You entered the wrong password!
login-correct-pin = Enter the Correct Pin.
login-correct-password = Enter the Correct Password.
login-save-password-failed = Could not save your password
login-save-pin-failed = Could not save your PIN

## Unlocking with the phone's fingerprint or screen lock

login-biometric-unlock = Unlock with Biometrics
# $label is the phone's own words for its prompt, such as "Use fingerprint".
login-biometric-title = { $label } to unlock Vector
login-biometric-changed-pin = Biometrics changed. Enter your PIN.
login-biometric-changed-password = Biometrics changed. Enter your password.
login-biometric-unavailable = Unlock unavailable right now. Restart Vector and try again.
login-biometric-recovery-state = Device security changed
login-biometric-recovery-title = Device Security Changed
# <b> is bold, <br> a line break.
login-biometric-recovery-text = This device's encrypted data can no longer be unlocked: its hardware key was invalidated (screen lock removed, or the data was moved to a new device).<br><br><b>Reset this device and sign in with your keys to restore from the network.</b>

## The welcome screen's footer

login-link-website = Website
login-link-docs = Documentation
login-link-privacy = Privacy Policy
login-link-donate = Donate
login-show-background = Show Background
login-hide-background = Hide Background

## Signing in with a remote signer

# $minutes and $seconds count down to when the link expires; $seconds always has two digits.
login-signer-waiting-time = Waiting for signer… ({ $minutes }:{ $seconds })
login-signer-waiting = Waiting for signer…
login-signer-bunker = Must start with bunker://
login-signer-connecting = Connecting to signer…
login-signer-approve = Check your signer app to approve…
login-signer-reconnected = Signer reconnected.
login-signer-connected = Connected. Choosing security…
login-signer-unreachable = Signer unreachable
login-signer-unreachable-text = Your remote signer didn't respond. If you've reset or revoked Vector's permissions in your signer app, re-pair below without losing your account data.
login-signer-reauthorize = Re-authorize Signer
login-signer-qr-tip = Pair with a QR code
login-signer-connect = Connect
login-signer-title = Login with Remote Signer
login-signer-reconnect-title = Reconnect Your Remote Signer
login-signer-generating = Generating connection link…
login-signer-copy-link = Copy Link
login-signer-copied = Copied

## Help and notices around signing in

login-methods-title = Ways to Sign In
# <b> is bold, <br> a line break. The three names match login-method-qr, -key and -signer.
login-methods-text = <b>QR Code</b><br>Use a device you're already signed in on. There, open Settings and choose Sign in on Another Device, then scan the code this device shows or type it in. Both devices confirm a number, so your keys pass between them without anyone else being able to read them.<br><br><b>Private Key</b><br>Paste your nsec or type your seed phrase. Good when you have your key backed up and no other device to hand.<br><br><b>Remote Signer</b><br>Keep your key in a signer app such as Amber or nsec.app, which approves what Vector signs. Paste its bunker:// link, or pair it by scanning the code Vector shows.
login-loading-error = Loading Error
login-signer-open-title = Approve in your signer
# <br> is a line break; $link is the page's address, shown as code.
login-signer-open-text = Your signer asked you to open this page. It opens in your browser, outside your private network.<br><code>{ $link }</code>
login-signer-offline = Remote signer offline. Please check your signer app.
login-signer-online = Remote signer back online.

## Why signing in or adding an account failed

login-invalid-nsec = Invalid nsec
login-invalid-seed = Invalid Seed Phrase
login-already-signed-in = Another account is already signed in.
login-password-empty = Password must not be empty.
login-external-signer = This account signs with an external signer, which this client cannot use yet.
login-no-private-key = No private key found
login-invalid-stored-key = Invalid stored key
# $error is why.
login-remove-failed = Failed to remove account directory: { $error }

## Why the remote signer couldn't be set up. "Bunker" is the remote signer's link.

login-signer-gone = The signer went away before it was set up
# $error is why.
login-signer-setup-failed = Bunker setup failed: { $error }
# $error is the signer's answer.
login-signer-no-pubkey-manual = Signer didn't return your pubkey. If you're in Manual mode, check your signer app for an approval prompt. ({ $error })
# $error is the signer's answer.
login-signer-no-pubkey = Signer didn't return your pubkey. Check the signer app for an approval prompt. ({ $error })
login-signer-not-remote = This account is not a remote-signer account.
login-signer-no-client-key = No client keypair loaded — please return to the login screen and try again
login-signer-other-identity = Signer returned a different identity. Re-authorize is only for the original account — logout to switch accounts.
# $error is why.
login-signer-save-failed = Failed to persist bunker URL: { $error }
login-signer-wrong-identity = Remote signer returned a different identity than this account. Either re-authorize from Settings, or logout and re-add the account.
