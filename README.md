# ChaloTrace — rugged-ledger-core

A supply chain ledger built for unreliable networks. Every handover of a product batch
(farm → transporter → warehouse → buyer) is a tiny, Ed25519-signed, hash-chained event
that a phone can queue offline and sync in one request when a 2G/3G signal appears.
Anyone can scan the final QR code and re-verify the whole history from the signatures.

## Status

Working today (all tests passing):

- Protobuf wire format (`proto/ledger.proto`)
- Signing, hash chaining and custody rules (`crates/ledger-core`)
- HTTP server: batch ingest + public verify endpoint (`crates/ledger-server`)
- Idempotent sync: resending a batch after a lost ack is safe

Not built yet: Postgres persistence (schema ready in `migrations/`), device key
registration/revocation, JS and Dart SDKs, field app, dashboard.

## How it works

| Event | Meaning |
|---|---|
| `GENESIS` | Batch created at origin. Signer becomes the first custodian. |
| `CHECKPOINT` | Current custodian records a place and time. |
| `HANDOFF` | Custodian names the next custodian's public key. |

Each event carries the BLAKE3 hash of the previous one and is signed by the current
custodian. The server rejects forged signers, sequence gaps, wrong `prev_hash`, a former
custodian writing after a handoff, and conflicting forks. `GET /api/v1/verify/{id}`
replays every signature from scratch on each call rather than trusting stored state.

## Run

```bash
cargo test --workspace
cargo run -p ledger-server          # listens on 0.0.0.0:8080 (LISTEN_ADDR to change)
curl localhost:8080/api/v1/verify/<32-hex-product-id>
```

## Payload size (measured)

One signed checkpoint is **177 bytes** as protobuf; a batch of 10 is 1,770 bytes. The
same event as JSON with hex-encoded keys/signatures is roughly 4x larger. The cost is
dominated by the cryptography (64-byte signature + 32-byte key + 32-byte prev hash), so
going much below ~100 bytes would mean registering device keys server-side and sending a
short key id instead. That is a planned optimization, not something v1 does.

## Known limits of v1

- Timestamps come from the device clock and are informational only.
- The ledger proves *who signed what, in what order*, not that the honey was really
  organic or the GPS reading honest. Origin trust still needs human/audit controls.
- Storage is in-memory; restarting the server loses data until Postgres is wired in.

## License

MIT
