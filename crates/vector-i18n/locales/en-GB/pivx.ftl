### PIVX: the wallet, sending a payment, and payments in chats.

## A payment in a chat

pivx-bubble-claim = Click to claim
# On a payment you sent, taking it back.
pivx-bubble-reclaim = Click to reclaim
pivx-bubble-syncing = Syncing...
pivx-bubble-confirming = Confirming...
pivx-bubble-claiming = Claiming...
pivx-bubble-claimed = Claimed
# Just claimed, this moment.
pivx-bubble-claimed-now = Claimed!
pivx-bubble-failed = Claim failed - tap to retry
# $amount is in PIV.
pivx-amount = { NUMBER($amount, minimumFractionDigits: 2, maximumFractionDigits: 2, useGrouping: "false") } PIV
# Its rough worth in the chosen currency; $currency is a code like "USD".
pivx-fiat = ~{ NUMBER($amount, minimumFractionDigits: 2, maximumFractionDigits: 2, useGrouping: "false") } { $currency }

## The wallet

pivx-send = Send
pivx-deposit = Deposit
pivx-withdraw = Withdraw
pivx-settings = Settings
# Tooltip on Deposit while the balance is over the limit.
pivx-deposit-off = Balance too high - please withdraw first
pivx-balance-high-title = Balance Too High
# <b> is bold, <br> a line break. $balance is the wallet's balance and $limit the recommended
# most, both in PIV.
pivx-balance-high-text = Your wallet balance is <b>{ NUMBER($balance, minimumFractionDigits: 2, maximumFractionDigits: 2, useGrouping: "false") } PIV</b>, which exceeds the recommended limit of { NUMBER($limit, useGrouping: "false") } PIV.<br><br>Vector is not intended to replace a proper cryptocurrency wallet. Please <b>withdraw your funds</b> to a secure wallet before depositing more.

## The wallet's dialogs

pivx-deposit-title = Deposit PIVX
pivx-send-title = Send PIVX
pivx-withdraw-title = Withdraw PIVX
pivx-settings-title = PIVX Wallet Settings

pivx-deposit-lede = Send PIVX to this address to deposit:
pivx-copy-address = Copy Address
pivx-address-copied = Address copied!
pivx-awaiting-deposit = Awaiting Deposit...
# $amount is in PIV, to eight decimal places.
pivx-received = Received { NUMBER($amount, minimumFractionDigits: 8, maximumFractionDigits: 8, useGrouping: "false") } PIV!
pivx-deposit-failed = Failed to create deposit address

pivx-loading = Loading...
pivx-no-funds = No funds available to send.
pivx-no-funds-hint = Deposit PIVX first.
pivx-load-failed = Failed to load wallet.
pivx-pick-amount = Select amount to send:
pivx-custom-amount = Send Custom Amount
pivx-custom-note = Custom amounts require on-chain confirmation and may take longer to send.
pivx-quick-send = Back to Quick Send
# <b> marks the amount, which is in PIV.
pivx-available = Available: <b>{ NUMBER($amount, minimumFractionDigits: 2, maximumFractionDigits: 2, useGrouping: "false") }</b> PIV
# <b> marks who it goes to: a name, or pivx-this-chat.
pivx-sending-to = Sending to: <b>{ $name }</b>
pivx-this-chat = this chat
pivx-sending = Sending...
pivx-send-to-chat = Send to Chat
pivx-destination = Destination Address
pivx-amount-label = Amount
pivx-fee = Network fee: ~0.0001 PIV
# Fills in the whole balance. Shown in capitals.
pivx-max = Max
pivx-withdrawing = Withdrawing...
pivx-auto-withdraw = Auto Withdraw Address
pivx-auto-withdraw-hint = If set, claimed PIVX will be sent directly to this address. Leave empty to keep funds in your Vector wallet.
pivx-currency = Display Currency
pivx-currency-hint = Choose your preferred fiat currency for balance display.
pivx-save-settings = Save Settings

## Toasts

pivx-open-chat-first = Open a chat first to send PIVX
pivx-pick-amount-first = Select an amount to send
pivx-enter-amount = Enter a valid amount
# $max is the most that can be sent, in PIV.
pivx-insufficient-max = Insufficient funds (max: { NUMBER($max, minimumFractionDigits: 2, maximumFractionDigits: 2, useGrouping: "false") } PIV)
pivx-insufficient = Insufficient balance
# $amount is in PIV.
pivx-sent = Sent { NUMBER($amount, minimumFractionDigits: 2, maximumFractionDigits: 2, useGrouping: "false") } PIV
# $error is why, as the wallet reported it.
pivx-send-failed = Failed to send: { $error }
pivx-invalid-address = Invalid PIVX address
pivx-invalid-address-format = Invalid PIVX address format
# $amount is in PIV.
pivx-withdrawn = Withdrawn { NUMBER($amount, minimumFractionDigits: 2, maximumFractionDigits: 2, useGrouping: "false") } PIV
# $error is why, as the wallet reported it.
pivx-withdraw-failed = Withdrawal failed: { $error }
pivx-settings-failed = Failed to save settings
pivx-settings-saved = Wallet settings saved
# $amount is in PIV.
pivx-claimed = Claimed { NUMBER($amount, minimumFractionDigits: 2, maximumFractionDigits: 2, useGrouping: "false") } PIV
# $error is why, as the wallet reported it.
pivx-claim-failed = Failed to claim: { $error }
pivx-not-ready = The wallet isn't ready yet
# Pays a payment address found in a message.
pivx-pay = Pay
