# ChaloTrace

**Trace where products really come from, even where the internet barely works.**

ChaloTrace is an open-source supply chain ledger for places with weak mobile networks, such as
rural Zambia. A farm cooperative creates a batch of honey, coffee or minerals. Each time the batch
changes hands or reaches a checkpoint (border post, warehouse, port), the person holding it records
a tiny digitally signed event on their phone. When a 2G/3G signal appears, the phone sends all
queued events in one small request. A buyer anywhere in the world can then scan the QR code on the
final product and see the verified journey.

## What is in this folder

```
ChaloTrace/
├── README.md                  <- you are here: what the project is
├── INSTALL.md                 <- step-by-step: install everything and launch the app
└── rugged-ledger-core/        <- the main open-source engine (this is the GitHub repo)
    ├── README.md              <- technical details of the engine
    ├── LICENSE                <- MIT open-source license
    ├── Cargo.toml             <- Rust workspace definition
    ├── .gitignore             <- files Git should skip
    ├── proto/
    │   └── ledger.proto       <- the compact data format devices use
    ├── migrations/
    │   └── 0001_init.sql      <- database tables, created automatically at server start
    └── crates/
        ├── ledger-core/       <- the engine: signing, hashing, custody rules
        │   └── src/  lib.rs, chain.rs, proto.rs
        └── ledger-server/     <- the web server
            ├── src/  main.rs, lib.rs, store.rs   (store.rs = PostgreSQL saving)
            └── examples/seed_demo.rs   <- demo: simulates a honey shipment
```

## Planned ecosystem (not built yet)

The original plan is a family of repositories. Only the first exists today:

| Repository | What it is | Status |
|---|---|---|
| `rugged-ledger-core` | Rust engine + server | **Built (v0.2, saves to PostgreSQL)** |
| `rugged-protobuf-definitions` | Shared data schemas, so others can track other goods | Planned (schema currently lives in `rugged-ledger-core/proto/`) |
| `rugged-ledger-js` | npm SDK | Planned |
| `rugged-ledger-dart` | Flutter SDK | Planned |
| ChaloTrace Field App, Cloud dashboard | Paid/commercial layer in the business plan | Planned |

## Business model in one paragraph

Open core: the engine, data schemas and basic SDKs stay free and open source. Money comes from a
hosted cloud version, compliance report generation (EU deforestation rules, Fair Trade, ESG audits),
a polished offline field app, enterprise features, paid pilot setups ($1,500–$3,000 per exporter in the
plan), and grants. Suggested tiers: Community $0, Cooperative Starter $49/mo, Exporter Pro $249/mo,
Enterprise $999+/mo. These numbers come from the planning notes and are untested assumptions.

## Roadmap

1. **Done:** signed, hash-chained event engine, batch sync, public verify endpoint.
2. **Done:** PostgreSQL storage; data survives restarts; tampering is detected at startup.
3. **Next:** device key registration, QR code generation, JS/Dart SDKs.
4. Field app and web dashboard; pilot with a Zambian cooperative.
5. Grants, paid pilots and hosted version.

## Honest limits

The ledger proves *who signed what, and in what order*. It cannot prove the honey was really
organic or that a GPS reading was honest, so real-world audits are still needed at the origin.
