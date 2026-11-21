## How each relay and server is reached on the network in use, in the lists and their dialogs,
## and the address a server keeps inside Tor or I2P. Tor and I2P are names.

route-connecting = Connecting
route-blocked = Blocked
route-off-i2p-only = Off in I2P-Only
route-unreachable = Unreachable
# $network is the network's name, like Tor or I2P.
route-needs = Needs { $network }
route-badge-onion = Onion
route-onion-failed = Onion address failed
route-i2p-failed = I2P address failed
route-connection = Connection
route-waiting = Waiting for the network to connect
# $network is the network's name, like Tor or I2P.
route-waiting-for = Waiting for { $network } to connect
route-alias-onion = Onion Address
route-alias-i2p = I2P Address
route-no-443-onion = This server doesn't use port 443, so it can't use an onion address.
route-no-443-i2p = This server doesn't use port 443, so it can't use an I2P address.
route-no-443-saved = This server doesn't use port 443, so this address isn't used.
route-built-in = Built into Vector.
# $host is the server's address, like relay.example.com.
route-checked = Checked. It serves { $host }.
# $host is the server's address, like relay.example.com.
route-check-failed = This address doesn't serve { $host }. Vector won't use it.
# $network is the network's name, like Tor or I2P.
route-unchecked = Not checked yet. Vector checks it when { $network } connects.
route-save = Save
route-remove = Remove
route-find = Find
route-check = Check
route-found-checked = Found and checked. Save to use it.
route-found = Found it. Save to use it.
