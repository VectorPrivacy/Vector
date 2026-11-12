# vector-lab

Live end-to-end checks of Vector bots, run against real relays. Each scenario starts a few bots
with fresh identities, each in its own process and data directory, has them talk through a
throwaway Community, checks what every bot saw, then dissolves the Community and deletes the
identities.

```sh
cargo run -p vector-lab -- all                      # every scenario
cargo run -p vector-lab -- events voice             # some of them
cargo run -p vector-lab -- all --relays wss://relay.example.com   # host the Community there
cargo run -p vector-lab -- gap --long               # also wait out the 5-minute history window
```

It exits non-zero when a check fails, so it can gate a release or a relay deploy.

| Scenario | What it checks |
| --- | --- |
| `events` | Joins, Roles and member Roles (with who and when), messages, reactions, edits and deletions in a Channel and in DMs: each event once, to the right bot. |
| `gap` | A bot restarting while a Channel is busy hears everything sent from its restart on, once, and reports Community changes made meanwhile. With `--long`, what was sent while it was away for longer stays history. |
| `voice` | Two voice Channels, one bot speaking a different tone into each, another hearing each call apart and knowing who spoke; leaving ends a call. |
| `flood` | Several bots posting at once: every message reaches every other bot once, with delivery latency. |

What it is good for:

- **Relay operators**: `--relays` puts the Community's planes on your relay, so the run shows
  whether Vector works there.
- **Bot authors**: a run tells whether the machine, network and relays a bot will use deliver.
- **Vector**: a release check of the SDK and `vector-core` against the live network.

Options: `--keep` leaves the working directory (logs, each bot's events) for a look afterwards;
`--bots N` and `--messages M` size the flood (4 and 25).
