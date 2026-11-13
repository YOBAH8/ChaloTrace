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
- PostgreSQL storage (`store.rs`): events saved before memory is updated; all chains are
  re-verified from signatures at startup, and the server refuses to start if any row was altered
- Database blocks UPDATE/DELETE on events (append-only trigger)

Not built yet: device key registration/revocation, JS and Dart SDKs, field app, dashboard.

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
export DATABASE_URL=postgres://chalotrace:chalotrace@localhost/chalotrace   # omit to run in memory only
cargo run -p ledger-server          # listens on 0.0.0.0:8080 (LISTEN_ADDR to change)
# in a second terminal, simulate a honey shipment and get a verify link:
cargo run -p ledger-server --example seed_demo
```

Full beginner install guide: see `../INSTALL.md`.

## File map

| File | Purpose |
|---|---|
| `proto/ledger.proto` | Wire format definition (source of truth for other-language SDKs) |
| `crates/ledger-core/src/proto.rs` | Rust types matching the .proto file |
| `crates/ledger-core/src/chain.rs` | Signing, hashing and custody/chain validation rules |
| `crates/ledger-core/src/lib.rs` | Library entry point, re-exports, and the engine's unit tests |
| `crates/ledger-server/src/lib.rs` | HTTP endpoints (ingest and verify) and server tests |
| `crates/ledger-server/src/store.rs` | Saves/loads chains in PostgreSQL; verifies them at startup |
| `crates/ledger-server/src/main.rs` | Starts the server (reads `DATABASE_URL`) |
| `crates/ledger-server/examples/seed_demo.rs` | Demo shipment script |
| `migrations/0001_init.sql` | PostgreSQL tables, run automatically at startup |

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
- Run ONE server process per database: the in-memory copy is the live view, so a second
  process writing to the same database would not see the first one's new events.
- The database tests (persistence, restart, tamper detection) were run by hand against
  PostgreSQL 16; the automated test suite uses the in-memory store.

## License

MIT
