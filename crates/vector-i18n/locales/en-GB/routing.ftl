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
