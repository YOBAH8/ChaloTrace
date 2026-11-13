//! DEMO SCRIPT: simulates a real shipment so you can see the app working.
//!
//! It plays three roles: a farmer in Mwinilunga creates a batch of honey, hands it to a driver,
//! and the driver records arrival at a Lusaka warehouse. The three signed events are sent to the
//! running server in ONE request (like a phone syncing after being offline), then it prints the
//! public verification link to open in your browser.
//!
//! Run (with the server already running in another terminal):
//!     cargo run -p ledger-server --example seed_demo

use ledger_core::{generate_key, sign_event, Chain, Event, EventKind, Message, SyncAck, SyncBatch};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let addr = std::env::var("SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

    let farmer = generate_key();
    let driver = generate_key();
    // Product id: first 16 bytes of the farmer's (random) public key, so every run is a new batch.
    let product_id: Vec<u8> = farmer.verifying_key().to_bytes()[..16].to_vec();

    // 1. Farmer creates the batch at the farm (Mwinilunga, North-Western Province).
    let genesis = sign_event(&farmer, Event {
        product_id: product_id.clone(), seq: 0, kind: EventKind::Genesis as i32,
        lat_e6: -11_733_300, lon_e6: 24_433_300, timestamp: now,
        note: "Organic honey, lot 7".into(), ..Default::default()
    });
    let mut chain = Chain::from_genesis(genesis.clone()).unwrap();

    // 2. Farmer hands custody to the driver.
    let handoff = sign_event(&farmer, Event {
        product_id: product_id.clone(), seq: 1, prev_hash: chain.head_hash().to_vec(),
        kind: EventKind::Handoff as i32, lat_e6: -11_733_300, lon_e6: 24_433_300, timestamp: now + 3600,
        next_actor: driver.verifying_key().to_bytes().to_vec(), note: "Loaded onto truck".into(), ..Default::default()
    });
    chain.append(handoff.clone()).unwrap();

    // 3. Driver records arrival in Lusaka.
    let arrival = sign_event(&driver, Event {
        product_id: product_id.clone(), seq: 2, prev_hash: chain.head_hash().to_vec(),
        kind: EventKind::Checkpoint as i32, lat_e6: -15_416_700, lon_e6: 28_283_300, timestamp: now + 86_400,
        note: "Arrived Lusaka warehouse".into(), ..Default::default()
    });

    let body = SyncBatch { events: vec![genesis, handoff, arrival] }.encode_to_vec();
    println!("Sending 3 signed events in one request ({} bytes total)...", body.len());

    let header = format!(
        "POST /api/v1/events HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/x-protobuf\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut stream = TcpStream::connect(&addr).unwrap_or_else(|_| {
        eprintln!("Could not reach the server at {addr}. Start it first: cargo run -p ledger-server");
        std::process::exit(1);
    });
    stream.write_all(header.as_bytes()).unwrap();
    stream.write_all(&body).unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    let split = response.windows(4).position(|w| w == b"\r\n\r\n").expect("bad HTTP response") + 4;
    let ack = SyncAck::decode(&response[split..]).expect("server did not return a SyncAck");
    println!("Server replied: {} accepted, {} duplicates, {} rejected", ack.accepted, ack.duplicates, ack.rejected.len());

    println!("\nOpen this link in your browser to see the verified history:");
    println!("  http://{addr}/api/v1/verify/{}", hex::encode(&product_id));
}
