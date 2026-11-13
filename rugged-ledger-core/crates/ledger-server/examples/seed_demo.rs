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

fn http_request(
    addr: &str,
    method: &str,
    path: &str,
    content_type: &str,
    authorization: Option<&str>,
    body: &[u8],
) -> Result<(u16, Vec<u8>), String> {
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(authorization) = authorization {
        request.push_str(&format!("Authorization: Bearer {authorization}\r\n"));
    }
    request.push_str("\r\n");
    let mut stream = TcpStream::connect(addr).map_err(|error| error.to_string())?;
    stream
        .write_all(request.as_bytes())
        .and_then(|_| stream.write_all(body))
        .map_err(|error| error.to_string())?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|error| error.to_string())?;
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "server returned a malformed HTTP response".to_owned())?;
    let headers = std::str::from_utf8(&response[..split])
        .map_err(|error| format!("invalid HTTP response headers: {error}"))?;
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| "server response has no HTTP status code".to_owned())?;
    Ok((status, response[split + 4..].to_vec()))
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1)
}

fn main() {
    let addr = std::env::var("SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let admin_token = std::env::var("ADMIN_TOKEN").unwrap_or_else(|_| {
        fail("Set ADMIN_TOKEN to enroll the demo farmer before running this example.")
    });
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let farmer = generate_key();
    let driver = generate_key();
    // Product id: first 16 bytes of the farmer's (random) public key, so every run is a new batch.
    let product_id: Vec<u8> = farmer.verifying_key().to_bytes()[..16].to_vec();
    let organization_body = format!(
        r#"{{"name":"Demo cooperative {}"}}"#,
        hex::encode(&product_id)
    );
    let (status, organization_body) = http_request(
        &addr,
        "POST",
        "/api/v1/admin/organizations",
        "application/json",
        Some(&admin_token),
        organization_body.as_bytes(),
    )
    .unwrap_or_else(|error| fail(&format!("Could not create demo organization: {error}")));
    if status != 201 {
        fail(&format!(
            "Could not create demo organization (HTTP {status})."
        ));
    }
    let organization: serde_json::Value = serde_json::from_slice(&organization_body)
        .unwrap_or_else(|error| fail(&format!("Invalid organization response: {error}")));
    let organization_id = organization["id"]
        .as_i64()
        .unwrap_or_else(|| fail("Organization response did not contain an id."));
    let device_body = format!(
        r#"{{"public_key":"{}"}}"#,
        hex::encode(farmer.verifying_key().to_bytes())
    );
    let (status, _) = http_request(
        &addr,
        "POST",
        &format!("/api/v1/admin/organizations/{organization_id}/devices"),
        "application/json",
        Some(&admin_token),
        device_body.as_bytes(),
    )
    .unwrap_or_else(|error| fail(&format!("Could not enroll demo farmer: {error}")));
    if status != 201 {
        fail(&format!("Could not enroll demo farmer (HTTP {status})."));
    }

    // 1. Farmer creates the batch at the farm (Mwinilunga, North-Western Province).
    let genesis = sign_event(
        &farmer,
        Event {
            product_id: product_id.clone(),
            seq: 0,
            kind: EventKind::Genesis as i32,
            lat_e6: -11_733_300,
            lon_e6: 24_433_300,
            timestamp: now,
            note: "Organic honey, lot 7".into(),
            ..Default::default()
        },
    );
    let mut chain = Chain::from_genesis(genesis.clone()).unwrap();

    // 2. Farmer hands custody to the driver.
    let handoff = sign_event(
        &farmer,
        Event {
            product_id: product_id.clone(),
            seq: 1,
            prev_hash: chain.head_hash().to_vec(),
            kind: EventKind::Handoff as i32,
            lat_e6: -11_733_300,
            lon_e6: 24_433_300,
            timestamp: now + 3600,
            next_actor: driver.verifying_key().to_bytes().to_vec(),
            note: "Loaded onto truck".into(),
            ..Default::default()
        },
    );
    chain.append(handoff.clone()).unwrap();

    // 3. Driver records arrival in Lusaka.
    let arrival = sign_event(
        &driver,
        Event {
            product_id: product_id.clone(),
            seq: 2,
            prev_hash: chain.head_hash().to_vec(),
            kind: EventKind::Checkpoint as i32,
            lat_e6: -15_416_700,
            lon_e6: 28_283_300,
            timestamp: now + 86_400,
            note: "Arrived Lusaka warehouse".into(),
            ..Default::default()
        },
    );

    let body = SyncBatch {
        events: vec![genesis, handoff, arrival],
    }
    .encode_to_vec();
    println!(
        "Sending 3 signed events in one request ({} bytes total)...",
        body.len()
    );

    let (status, response) = http_request(
        &addr,
        "POST",
        "/api/v1/events",
        "application/x-protobuf",
        None,
        &body,
    )
    .unwrap_or_else(|error| fail(&format!("Could not reach the server at {addr}: {error}")));
    if status != 200 {
        fail(&format!("Event sync failed with HTTP {status}."));
    }
    let ack = SyncAck::decode(response.as_slice())
        .unwrap_or_else(|error| fail(&format!("Server did not return a SyncAck: {error}")));
    println!(
        "Server replied: {} accepted, {} duplicates, {} rejected",
        ack.accepted,
        ack.duplicates,
        ack.rejected.len()
    );

    println!("\nOpen this link in your browser to see the verified history:");
    println!("  http://{addr}/api/v1/verify/{}", hex::encode(&product_id));
}
