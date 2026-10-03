//! Device transfer: move an account to another device by scanning a QR or typing a short code.
//! Both devices meet on relays, prove the code to each other with a PAKE, and the key moves only
//! after the user approves on the device that holds it.

pub mod avatar;
pub mod code;
pub mod crypto;
pub mod service;
pub mod session;
pub mod wire;

#[cfg(test)]
mod relay_probe {
    use futures_util::StreamExt;
    use crate::ClientRelayExt;
    use nostr_sdk::prelude::*;
    use std::time::Duration;

    /// Live check that each trusted relay carries the transfer kind between two fresh keys, and
    /// whether it replays it to a later subscriber. `cargo test -p vector-core relay_probe -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn every_trusted_relay_carries_an_ephemeral_transfer_event() {
        for url in crate::state::TRUSTED_RELAYS {
            let room = Keys::generate().public_key();
            let filter = Filter::new().kind(Kind::Custom(24681)).pubkey(room).since(Timestamp::now() - 60);

            let listener = Client::builder().build();
            listener.add_managed_relay(*url).await.unwrap();
            listener.connect().and_wait(Duration::from_secs(10)).await;
            let mut notes = listener.notifications();
            listener.subscribe(filter.clone()).await.unwrap();

            let sender_keys = Keys::generate();
            let sender = Client::builder().build();
            sender.add_managed_relay(*url).await.unwrap();
            sender.connect().and_wait(Duration::from_secs(10)).await;
            tokio::time::sleep(Duration::from_millis(800)).await;
            // As large as the biggest real message (a sender's hello with its avatar), with room to spare.
            let event = EventBuilder::new(Kind::Custom(24681), "x".repeat(32 * 1024))
                .tag(Tag::public_key(room))
                .finalize(&sender_keys)
                .unwrap();
            let sent = sender.send_event(&event).await;

            let got = tokio::time::timeout(Duration::from_secs(10), async {
                while let Some(n) = notes.next().await {
                    if let ClientNotification::Event { event: e, .. } = n {
                        if e.id == event.id { return true; }
                    }
                }
                false
            }).await.unwrap_or(false);

            let late = Client::builder().build();
            late.add_managed_relay(*url).await.unwrap();
            late.connect().and_wait(Duration::from_secs(10)).await;
            let replayed = late.fetch_events(filter).timeout(Duration::from_secs(6)).await
                .map(|evs| evs.iter().any(|e| e.id == event.id)).unwrap_or(false);

            println!("{url}: publish={:?} live={got} replayed_to_late={replayed}",
                sent.as_ref().map(|o| (o.success.len(), o.failed.values().cloned().collect::<Vec<_>>())).map_err(|e| e.to_string()));
            for c in [listener, sender, late] { c.shutdown().await; }
        }
    }
}
