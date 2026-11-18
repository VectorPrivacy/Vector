### The network the account's traffic goes through, and stepping outside it.

## Asked before calls, voice and multiplayer Mini Apps connect directly. <br> is a line break.

# $network is the network chosen in Settings, such as Tor or I2P.
outside-connect-title = Connect outside { $network }?
# Before the app knows which network is chosen.
outside-connect-title-unknown = Connect outside the network in use?
outside-connect-text = Calls and multiplayer Mini Apps connect directly.<br>The people you connect with and the iroh relay can see your IP address.<br>Allowed until you close Vector, switch accounts or change networks.
outside-connect = Connect

## Asked before a web link opens in the browser, off Clearnet. <br> is a line break.

outside-browser-title = Open in your browser?
# $network is the network chosen in Settings, such as Tor or I2P; $url is the link.
outside-browser-text = It loads outside { $network }.<br><br>{ $url }
# Before the app knows which network is chosen. $url is the link.
outside-browser-text-unknown = It loads outside the network in use.<br><br>{ $url }
outside-browser-open = Open
outside-browser-copy = Copy Link
# $count is how many relays you use.
network-relays-unreachable = { $count ->
    [one] Couldn't reach your relay, new messages may be missing
   *[other] Couldn't reach any of your { $count } relays, new messages may be missing
}
