# Zapstore publishing through a bunker

`distribute.yaml` publishes to Zapstore with `zsp`, which signs the release events with whatever
`ZAPSTORE_SIGN_WITH` holds. It must never hold the account nsec. A NIP-46 bunker keeps the key on
a machine you own and hands GitHub a connection secret that can be rotated on its own.

## What the bunker signs

Per release, zsp signs a few events as the app author: the app (kind 32267), the release
(30063) and file metadata (3063). Nothing else needs the key.

## Amber on your phone (what Vector uses)

Amber is the bunker: the key lives on the phone, and each event kind is approved once.

1. In Amber, add a new application and pick the bunker (remote signer) option. Under relays,
   keep `relay.nsec.app` or add `wss://jskitty.com/nostr`; the runner needs to reach one of
   them. Copy the `bunker://` URI it shows.
2. From your laptop, prove signing works before a release depends on it:

   ```bash
   SIGN_WITH='bunker://...' zsp publish --offline --quiet zapstore.yaml
   ```

   `--offline` signs every event and publishes nothing. Amber prompts for kinds 32267, 30063 and 3063. Approve each with "remember" so CI never
   waits on a tap. Do not grant "sign everything" to this app: the per-kind grant is the
   whole point.
3. Add the URI as the `ZAPSTORE_SIGN_WITH` repository secret.

Turn battery optimisation off for Amber. If the phone is unreachable when a release publishes,
the job fails cleanly; re-run it from the Actions tab. Revoke by deleting the app in Amber.

## Alternative: nak bunker on the relay box

`nak bunker` is a single static binary and a one-line service. It signs any kind for a client
holding the authorized secret, so treat that secret like a password: it lives only in the
GitHub secret, and rotating it is one edit here.

1. Install `nak` (https://github.com/fiatjaf/nak/releases) to `/usr/local/bin/nak`.

2. Put the key where only root reads it. Generate the client secret alongside it:

   ```bash
   sudo install -m 600 /dev/null /etc/nak-bunker.env
   sudo tee /etc/nak-bunker.env >/dev/null <<EOF
   NAK_SEC=nsec1...
   NAK_CLIENT_SECRET=$(openssl rand -hex 16)
   EOF
   ```

3. Service. The relay is yours, so the bunker and the runner meet on infrastructure you control;
   a second public relay is there for when yours is restarting.

   ```ini
   # /etc/systemd/system/nak-bunker.service
   [Unit]
   Description=NIP-46 bunker for Zapstore publishing
   After=network-online.target
   Wants=network-online.target

   [Service]
   EnvironmentFile=/etc/nak-bunker.env
   ExecStart=/usr/local/bin/nak bunker --sec ${NAK_SEC} -s ${NAK_CLIENT_SECRET} \
       wss://jskitty.com/nostr wss://relay.nsec.app
   Restart=always
   RestartSec=5
   DynamicUser=yes
   NoNewPrivileges=yes
   ProtectSystem=strict
   ProtectHome=yes
   PrivateTmp=yes

   [Install]
   WantedBy=multi-user.target
   ```

   ```bash
   sudo systemctl daemon-reload
   sudo systemctl enable --now nak-bunker
   sudo journalctl -u nak-bunker -n 20
   ```

   The log prints the `bunker://<pubkey>?relay=...&secret=...` URI. The secret in it is
   `NAK_CLIENT_SECRET`.

4. Prove it from your laptop before trusting it with a release:

   ```bash
   SIGN_WITH='bunker://...' zsp publish --offline --quiet zapstore.yaml
   ```

5. Add the URI as the `ZAPSTORE_SIGN_WITH` repository secret. The next stable release publishes
   itself.

## Rotating or revoking

Change `NAK_CLIENT_SECRET`, restart the service, update the GitHub secret. The old URI is dead
the moment the service restarts. Your nsec never moved.

## Alternative with kind scoping, server-side

`nsecbunkerd` (https://github.com/kind-0/nsecbunkerd) grants each connected app a policy, so
the Zapstore grant can be limited to kinds 32267, 30063 and 3063 and nothing else. It is a Node
service with an admin flow rather than one binary. The GitHub side is identical: the
`bunker://` URI it issues goes into the same secret.
