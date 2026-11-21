## Settings > Privacy > Routing: a pill per network, the card of the one on view, then that
## network's own settings. Tor and I2P are names and stay as they are.

routing-standard = Standard
routing-in-use = In use
routing-choose = Choose how Vector connects.
# $network is the network's name, like Tor or I2P.
routing-missing = This build doesn't include { $network }.
routing-direct-note = Connects straight to relays and servers.
routing-connected = Connected.
# $percent is a whole number from 0 to 100.
routing-bootstrapping-at = Bootstrapping { $percent }%…
routing-bootstrapping = Bootstrapping…
routing-failed = Failed to start.
routing-starting = Starting…
routing-connected-inside = Connected inside I2P only.
routing-finding-router = Looking for your router…
routing-building-tunnels = Building tunnels…
routing-not-in-use = Not in use.
routing-held-call = End the call to change networks.
routing-held-tor = Wait for Tor to finish connecting.
# $seconds is a whole number of seconds.
routing-retrying-in = Retrying in { $seconds }s.
routing-retry-now = Retry now
routing-retry = Retry
routing-choose-title = Choose a Network
routing-tor-title = Route traffic through Tor
routing-direct-title = Direct Connection
# $network is the network's name, like I2P.
routing-route-through = Route traffic through { $network }
# Read out by screen readers. $network is the network's name, like Tor or I2P, or the word for Standard.
routing-spoken-connected = { $network } connected.
# Read out by screen readers. $network is the network's name, like Tor or I2P, or the word for Standard.
routing-spoken-failed = { $network } failed to start.
# Read out by screen readers. $network is the network's name, like Tor or I2P, or the word for Standard.
routing-spoken-connecting = { $network } connecting.
routing-visit-tor = Visit torproject.org
# $network is the network's name, like Tor or I2P, or the word for Standard.
routing-use = Use { $network }

## Tor

routing-tor-external = Use Running Tor
routing-save = Save
routing-find = Find
routing-port = Port
# $port is a port number.
routing-found-tor-browser = Found Tor Browser on port { $port }.
# $port is a port number.
routing-found-tor = Found Tor on port { $port }.
routing-no-tor = No Tor found. Open Tor Browser or start tor.
routing-bridges = Use Bridges
routing-multi-circuit = Multi-Circuit
routing-bridges-none = No bridges configured.
# $count is how many bridges are saved.
routing-bridges-count = { $count ->
    [one] { $count } bridge configured
   *[other] { $count } bridges configured
}
routing-obfs4-missing = obfs4 bridges need obfs4proxy installed. Saving will fail until it's available.
routing-bridges-hint = One bridge per line. obfs4 recommended (bypasses DPI). Vanilla IP:port fingerprint also works for private bridges.
routing-bridges-get = Get bridges at bridges.torproject.org.
routing-apply-reconnect = Apply & Reconnect
routing-circuit-new-tip = Build a new circuit
routing-circuit-new = New
routing-circuits = Circuits
routing-circuit-active = Active Circuit
routing-circuits-external = That Tor builds the circuits. New gives every server a fresh one.
# Under the number of open circuits, which shows larger beside it. $count is that number.
routing-circuits-active = { $count ->
    [one] active circuit
   *[other] active circuits
}
# $hosts is how many relays and servers the circuits serve.
routing-circuits-across = { $hosts ->
    [one] across { $hosts } host. Each one's circuit is in its info.
   *[other] across { $hosts } relays and servers. Each one's circuit is in its info.
}
routing-circuit-building = Building circuit…
# $error is the reason, as the network gave it.
routing-circuit-failed = Failed: { $error }
routing-circuit-none = No active circuit.
routing-hop-guard = Guard
routing-hop-middle = Middle
routing-hop-exit = Exit
routing-hop-onion = Onion
routing-hop-rendezvous = Rendezvous
# A circuit hop that is also a bridge. $position is the hop's name, like Guard.
routing-hop-bridge = { $position } · Bridge

## I2P

routing-connection = Connection
routing-test = Test
routing-router = Router
# $user is the user name the router knows Vector by.
routing-signs-in-as = Signs in as { $user }.
routing-remove = Remove
routing-sam-password = SAM Password
routing-username = Username
routing-password = Password
routing-add-sam-password = Add SAM Password
routing-address = Address
routing-address-account = your relays
routing-address-shared = other servers
routing-new-address = New Address
routing-renew = Renew
routing-i2p-only = I2P-Only
routing-stranded = None of your relays are on I2P.
routing-open-network = Open Network
routing-only-note = Only I2P servers and I2P addresses you added.
routing-outproxies = Outproxies
routing-reset = Reset
routing-move-up = Move up
routing-move-down = Move down
routing-stale = Last known. I2P isn't connected.
routing-on = On
routing-off = Off
routing-name = Name
routing-add-outproxy = Add Outproxy
# $count is the most outproxies allowed.
routing-outproxies-max = You can add up to { $count } outproxies.
routing-foot = Works with i2pd or Java I2P with SAM turned on.
routing-health-untried = Not tried
# $minutes is a whole number of minutes.
routing-health-resting = Resting { $minutes }m
routing-health-failing = Failing
routing-health-ok = OK
# $ports is a list of port numbers, like "80, 443".
routing-health-ok-except = OK, not port { $ports }
# $ports is a list of port numbers, like "80, 443".
routing-health-refuses = Refuses port { $ports }
routing-health-unreached = Not reached yet

## The line over the chat list while the network holds traffic back. The pane is narrow, so
## these are short.

routing-notice-router-unreachable = I2P router not answering.
routing-notice-sam-refused = That port isn't an I2P router.
routing-notice-sam-too-old = I2P router too old for Vector.
routing-notice-sam-auth-required = I2P router asks for a password.
routing-notice-sam-auth-failed = I2P router refused the password.
routing-notice-session-failed = I2P router couldn't open a session.
routing-notice-session-lost = Reconnecting to I2P.
routing-notice-tor-failed = Tor couldn't connect.
# $network is the network's name, like Tor or I2P.
routing-notice-not-starting = { $network } isn't starting.
routing-notice-unnamed-not-starting = The network isn't starting.
routing-notice-stranded = I2P-Only is on and none of your relays are on I2P.
routing-notice-no-outproxy = No outproxy is reachable right now.

## Routing's popups, toasts and notes

routing-new-address-toast = New I2P address in use.
routing-port-auto = Auto
routing-i2p-only-ask = Turn on I2P-Only?
routing-i2p-only-ask-text = None of your relays are on I2P. Turn on I2P-Only anyway?
routing-i2p-only-ask-confirm = Turn On
# $name is the outproxy's name.
routing-outproxy-remove-ask = Remove { $name }?
routing-outproxy-remove-text = Vector stops using this outproxy.
routing-outproxies-reset-ask = Use the built-in outproxies?
routing-outproxies-reset-own = Your own outproxies are removed.
routing-outproxies-reset-order = The built-in order comes back.
routing-address-copied = Address copied
# $port is a port number.
routing-no-router = No I2P router answered on port { $port }.
routing-switch-i2p-ask = Switch to I2P?
# $reason is why the router check failed, in one or more sentences.
routing-switch-i2p-text = { $reason } Switch anyway?
routing-switch = Switch
routing-keep-realtime-ask = Keep your Mini App session?
# $network is the network's name, like Tor or I2P.
routing-keep-realtime-text = It connects directly, outside { $network }.
# $network is the network's name, like Tor or I2P.
routing-keep-realtime-end = End it to keep everything on { $network }.
routing-keep = Keep
routing-end = End
routing-bridges-add-apply = Add bridge lines, then Apply.
routing-bridges-add-save = Add bridge lines, then Save.
routing-bridges-enabling = Enabling bridges, reconnecting…
routing-bridges-disabling = Disabling bridges, reconnecting…
routing-bridges-applying = Applying & reconnecting…
routing-bridges-saving = Saving…
routing-bridges-enabled = Bridges enabled. Tor reconnected.
routing-bridges-disabled = Bridges disabled. Tor reconnected directly.
routing-bridges-on = Bridges on. Tor will connect through them.
routing-bridges-off = Bridges off. Tor will connect directly.
routing-bridges-saved = Bridges saved. Tor will use them.
routing-bridges-applied = Bridges applied. Tor reconnected.
routing-port-range = Enter a port from 1 to 65535.

## Why a network change was refused

routing-pref-not-loaded = The network preference isn't loaded.
routing-change-before-login = The network can only be changed here before signing in.
routing-finish-signer = Finish or cancel the signer connection first.
routing-end-call = End the call to change networks.
routing-save-failed = Couldn't save the network choice. It applies until Vector restarts.
routing-alias-needs-network = Use Tor or I2P to check this address.
routing-alias-own-relays = Vector looks these up only for your own relays.
routing-tor-before-login = Tor can only be switched here before signing in.
routing-circuit-timeout = Timed out building/reading circuit (20s)
