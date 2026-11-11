# vector-push

Sealed push notifications for [Vector](https://vectorapp.io). A sender encrypts the notice to the
recipient's device (Web Push, RFC 8291), and a blind pusher forwards it without being able to read
it or tell who it's from.

`vector-core` uses this to wake a recipient's devices for a DM. You'd use it directly only to run a
pusher of your own.

License: MIT.
