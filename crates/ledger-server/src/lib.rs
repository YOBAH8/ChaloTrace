//! HTTP surface (storage is in store.rs):
//!   POST /api/v1/events           body: protobuf SyncBatch -> protobuf SyncAck
//!   GET  /api/v1/verify/:product  public JSON timeline, re-verified from signatures on every call
//!   GET  /health

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use ledger_core::{open_event, AppendOutcome, Chain, EventKind, Message, Rejection, SyncAck, SyncBatch};
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::Mutex;

pub mod store;
pub use store::Store;

pub const MAX_BATCH_EVENTS: usize = 500;

#[derive(Clone)]
pub struct AppState {
    store: Arc<Mutex<Store>>,
}

/// Server with no database (data is lost on restart). Used by tests.
pub fn app() -> Router {
    app_with(Store::in_memory())
}

/// Server backed by the given store (in-memory or PostgreSQL).
pub fn app_with(store: Store) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/v1/events", post(ingest))
        .route("/api/v1/verify/:product_id", get(verify))
        .with_state(AppState { store: Arc::new(Mutex::new(store)) })
}

async fn health(State(st): State<AppState>) -> String {
    format!("ok ({} products)", st.store.lock().await.product_count())
}

fn proto_response(ack: SyncAck) -> Response {
    ([(header::CONTENT_TYPE, "application/x-protobuf")], ack.encode_to_vec()).into_response()
}

async fn ingest(State(st): State<AppState>, body: Bytes) -> Response {
    let batch = match SyncBatch::decode(body) {
        Ok(b) => b,
        Err(_) => return (StatusCode::BAD_REQUEST, "body is not a SyncBatch").into_response(),
    };
    if batch.events.len() > MAX_BATCH_EVENTS {
        return (StatusCode::PAYLOAD_TOO_LARGE, "batch too large").into_response();
    }
    let mut ack = SyncAck::default();
    let mut store = st.store.lock().await;
    for (i, se) in batch.events.into_iter().enumerate() {
        match store.apply(se).await {
            Ok(AppendOutcome::Appended) => ack.accepted += 1,
            Ok(AppendOutcome::Duplicate) => ack.duplicates += 1,
            Err(reason) => ack.rejected.push(Rejection { index: i as u32, reason }),
        }
    }
    proto_response(ack)
}

#[derive(Serialize)]
struct EventView {
    seq: u32,
    kind: &'static str,
    actor: String,
    lat: f64,
    lon: f64,
    timestamp: u64,
    note: String,
    hash: String,
    next_actor: Option<String>,
}

#[derive(Serialize)]
struct VerifyView {
    product_id: String,
    verified: bool,
    current_custodian: String,
    head_hash: String,
    events: Vec<EventView>,
}

async fn verify(State(st): State<AppState>, Path(product_id): Path<String>) -> Response {
    let pid: [u8; 16] = match hex::decode(&product_id).ok().and_then(|v| v.try_into().ok()) {
        Some(p) => p,
        None => return (StatusCode::BAD_REQUEST, "product_id must be 32 hex characters").into_response(),
    };
    let stored = match st.store.lock().await.get(&pid) {
        Some(c) => c.events().to_vec(),
        None => return (StatusCode::NOT_FOUND, "unknown product").into_response(),
    };
    // Never trust the cache: replay every signature and link from scratch.
    let chain = match Chain::verify(&stored) {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("stored chain failed verification: {e}")).into_response(),
    };
    let events = chain.events().iter().enumerate().map(|(i, se)| {
        let ev = open_event(se).expect("already verified");
        EventView {
            seq: ev.seq,
            kind: match EventKind::try_from(ev.kind) {
                Ok(EventKind::Genesis) => "GENESIS",
                Ok(EventKind::Checkpoint) => "CHECKPOINT",
                Ok(EventKind::Handoff) => "HANDOFF",
                _ => "UNKNOWN",
            },
            actor: hex::encode(&ev.actor_key),
            lat: ev.lat_e6 as f64 / 1e6,
            lon: ev.lon_e6 as f64 / 1e6,
            timestamp: ev.timestamp,
            note: ev.note,
            hash: hex::encode(chain.hash_at(i)),
            next_actor: (!ev.next_actor.is_empty()).then(|| hex::encode(&ev.next_actor)),
        }
    }).collect();
    Json(VerifyView {
        product_id,
        verified: true,
        current_custodian: hex::encode(chain.custodian()),
        head_hash: hex::encode(chain.head_hash()),
        events,
    }).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use ledger_core::{sign_event, Event, SignedEvent, SigningKey};
    use tower::ServiceExt;

    async fn post_batch(app: &Router, events: Vec<SignedEvent>) -> SyncAck {
        let req = Request::post("/api/v1/events").body(Body::from(SyncBatch { events }.encode_to_vec())).unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        SyncAck::decode(res.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }

    #[tokio::test]
    async fn offline_batch_sync_then_public_verify() {
        let app = app();
        let farmer = SigningKey::from_bytes(&[1; 32]);
        let driver = SigningKey::from_bytes(&[2; 32]);
        let pid = [7u8; 16];

        let g = sign_event(&farmer, Event { product_id: pid.to_vec(), kind: EventKind::Genesis as i32, lat_e6: -12_800_000, lon_e6: 28_200_000, note: "honey".into(), ..Default::default() });
        let mut local = Chain::from_genesis(g.clone()).unwrap();
        // Device queues two events offline, then syncs everything in one request.
        let h = sign_event(&farmer, Event { product_id: pid.to_vec(), seq: 1, prev_hash: local.head_hash().to_vec(), kind: EventKind::Handoff as i32, next_actor: driver.verifying_key().to_bytes().to_vec(), ..Default::default() });
        local.append(h.clone()).unwrap();
        let cp = sign_event(&driver, Event { product_id: pid.to_vec(), seq: 2, prev_hash: local.head_hash().to_vec(), kind: EventKind::Checkpoint as i32, lat_e6: -15_416_700, lon_e6: 28_283_300, note: "Lusaka warehouse".into(), ..Default::default() });

        let ack = post_batch(&app, vec![g.clone(), h.clone(), cp.clone()]).await;
        assert_eq!((ack.accepted, ack.duplicates, ack.rejected.len()), (3, 0, 0));

        // Ack was lost, device resends: everything is a duplicate, nothing breaks.
        let ack = post_batch(&app, vec![g, h, cp.clone()]).await;
        assert_eq!((ack.accepted, ack.duplicates, ack.rejected.len()), (0, 3, 0));

        // Forged event from a stranger is rejected.
        let evil = SigningKey::from_bytes(&[66; 32]);
        let forged = sign_event(&evil, Event { product_id: pid.to_vec(), seq: 3, prev_hash: local.head_hash().to_vec(), kind: EventKind::Checkpoint as i32, ..Default::default() });
        let ack = post_batch(&app, vec![forged]).await;
        assert_eq!(ack.rejected.len(), 1);

        let res = app.clone().oneshot(Request::get(format!("/api/v1/verify/{}", hex::encode(pid))).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: serde_json::Value = serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(v["verified"], true);
        assert_eq!(v["events"].as_array().unwrap().len(), 3);
        assert_eq!(v["events"][2]["note"], "Lusaka warehouse");
        assert_eq!(v["current_custodian"], hex::encode(driver.verifying_key().to_bytes()));
    }

    #[tokio::test]
    async fn unknown_product_and_bad_id() {
        let app = app();
        let res = app.clone().oneshot(Request::get(format!("/api/v1/verify/{}", "00".repeat(16))).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let res = app.oneshot(Request::get("/api/v1/verify/zzz").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }
}
