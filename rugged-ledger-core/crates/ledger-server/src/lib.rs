//! HTTP surface (storage is in store.rs):
//!   POST /api/v1/events           body: protobuf SyncBatch -> protobuf SyncAck
//!   GET  /api/v1/verify/:product  public JSON timeline, re-verified from signatures on every call
//!   GET  /health

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use ledger_core::{
    open_event, AppendOutcome, Chain, EventKind, Message, Rejection, SyncAck, SyncBatch,
};
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::Mutex;

pub mod store;
pub use store::Store;

pub const MAX_BATCH_EVENTS: usize = 500;

#[derive(Clone)]
pub struct AppState {
    store: Arc<Mutex<Store>>,
    admin_token: Option<Arc<str>>,
}

/// Server with no database (data is lost on restart). Used by tests.
pub fn app() -> Router {
    app_with(Store::in_memory())
}

/// Server backed by the given store (in-memory or PostgreSQL).
pub fn app_with(store: Store) -> Router {
    app_with_admin_token(store, None)
}

/// Server backed by a store with optional authorization for administrative endpoints.
pub fn app_with_admin_token(store: Store, admin_token: Option<String>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/v1/events", post(ingest))
        .route("/api/v1/verify/:product_id", get(verify))
        .route("/api/v1/admin/organizations", post(create_organization))
        .route(
            "/api/v1/admin/organizations/:organization_id/devices",
            post(enroll_device),
        )
        .route(
            "/api/v1/admin/devices/:public_key",
            axum::routing::delete(revoke_device),
        )
        .with_state(AppState {
            store: Arc::new(Mutex::new(store)),
            admin_token: admin_token.map(Arc::from),
        })
}

async fn health(State(st): State<AppState>) -> String {
    format!("ok ({} products)", st.store.lock().await.product_count())
}

fn proto_response(ack: SyncAck) -> Response {
    (
        [(header::CONTENT_TYPE, "application/x-protobuf")],
        ack.encode_to_vec(),
    )
        .into_response()
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
            Err(reason) => ack.rejected.push(Rejection {
                index: i as u32,
                reason,
            }),
        }
    }
    proto_response(ack)
}

#[derive(serde::Deserialize)]
struct CreateOrganization {
    name: String,
}

#[derive(Serialize)]
struct OrganizationCreated {
    id: i64,
}

async fn create_organization(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<CreateOrganization>,
) -> Response {
    if let Err(response) = authorize_admin(&st, &headers) {
        return (response.0, response.1).into_response();
    }
    match st.store.lock().await.create_organization(&input.name).await {
        Ok(id) => (StatusCode::CREATED, Json(OrganizationCreated { id })).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error).into_response(),
    }
}

#[derive(serde::Deserialize)]
struct EnrollDevice {
    public_key: String,
}

#[derive(Serialize)]
struct DeviceEnrolled {
    organization_id: i64,
    public_key: String,
}

async fn enroll_device(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(organization_id): Path<i64>,
    Json(input): Json<EnrollDevice>,
) -> Response {
    if let Err(response) = authorize_admin(&st, &headers) {
        return (response.0, response.1).into_response();
    }
    let public_key: [u8; 32] = match hex::decode(&input.public_key)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
    {
        Some(key) => key,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "public_key must be 64 hex characters",
            )
                .into_response()
        }
    };
    match st
        .store
        .lock()
        .await
        .enroll_device(organization_id, public_key)
        .await
    {
        Ok(()) => (
            StatusCode::CREATED,
            Json(DeviceEnrolled {
                organization_id,
                public_key: hex::encode(public_key),
            }),
        )
            .into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error).into_response(),
    }
}

async fn revoke_device(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(public_key): Path<String>,
) -> Response {
    if let Err(response) = authorize_admin(&st, &headers) {
        return (response.0, response.1).into_response();
    }
    let public_key: [u8; 32] = match hex::decode(&public_key)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
    {
        Some(key) => key,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "public_key must be 64 hex characters",
            )
                .into_response()
        }
    };
    match st.store.lock().await.revoke_device(public_key).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "unknown device key").into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error).into_response(),
    }
}

fn authorize_admin(st: &AppState, headers: &HeaderMap) -> Result<(), (StatusCode, &'static str)> {
    let Some(token) = st.admin_token.as_deref() else {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "admin API is disabled"));
    };
    let provided = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if provided.is_some_and(|provided| constant_time_eq(provided.as_bytes(), token.as_bytes())) {
        Ok(())
    } else {
        Err((StatusCode::UNAUTHORIZED, "invalid admin token"))
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(
            left.get(index).copied().unwrap_or_default()
                ^ right.get(index).copied().unwrap_or_default(),
        );
    }
    difference == 0
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
    actor_enrolled: bool,
    next_actor_enrolled: Option<bool>,
}

#[derive(Serialize)]
struct VerifyView {
    product_id: String,
    verified: bool,
    current_custodian: String,
    current_custodian_enrolled: bool,
    head_hash: String,
    events: Vec<EventView>,
}

async fn verify(State(st): State<AppState>, Path(product_id): Path<String>) -> Response {
    let pid: [u8; 16] = match hex::decode(&product_id)
        .ok()
        .and_then(|v| v.try_into().ok())
    {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "product_id must be 32 hex characters",
            )
                .into_response()
        }
    };
    let stored = match st.store.lock().await.get(&pid) {
        Some(c) => c.events().to_vec(),
        None => return (StatusCode::NOT_FOUND, "unknown product").into_response(),
    };
    // Never trust the cache: replay every signature and link from scratch.
    let chain = match Chain::verify(&stored) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("stored chain failed verification: {e}"),
            )
                .into_response()
        }
    };
    let mut keys = std::collections::HashSet::new();
    keys.insert(chain.custodian());
    for signed_event in chain.events() {
        let event = match open_event(signed_event) {
            Ok(event) => event,
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("stored chain failed verification: {error}"),
                )
                    .into_response()
            }
        };
        let actor: [u8; 32] = match event.actor_key.as_slice().try_into() {
            Ok(actor) => actor,
            Err(_) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "stored actor key has wrong length",
                )
                    .into_response()
            }
        };
        keys.insert(actor);
        if !event.next_actor.is_empty() {
            let next_actor: [u8; 32] = match event.next_actor.as_slice().try_into() {
                Ok(next_actor) => next_actor,
                Err(_) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "stored next actor key has wrong length",
                    )
                        .into_response()
                }
            };
            keys.insert(next_actor);
        }
    }
    let enrolled = match st.store.lock().await.registered_device_keys(&keys).await {
        Ok(enrolled) => enrolled,
        Err(error) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, error).into_response();
        }
    };
    let events = match event_views(&chain, &enrolled) {
        Ok(events) => events,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("stored chain failed verification: {error}"),
            )
                .into_response()
        }
    };
    Json(VerifyView {
        product_id,
        verified: true,
        current_custodian: hex::encode(chain.custodian()),
        current_custodian_enrolled: enrolled.contains(&chain.custodian()),
        head_hash: hex::encode(chain.head_hash()),
        events,
    })
    .into_response()
}

fn event_views(
    chain: &Chain,
    enrolled: &std::collections::HashSet<[u8; 32]>,
) -> Result<Vec<EventView>, String> {
    chain
        .events()
        .iter()
        .enumerate()
        .map(|(i, signed_event)| {
            let event = open_event(signed_event).map_err(|error| error.to_string())?;
            let actor: [u8; 32] = event
                .actor_key
                .as_slice()
                .try_into()
                .map_err(|_| "stored actor key has wrong length".to_owned())?;
            let next_actor: Option<[u8; 32]> = if event.next_actor.is_empty() {
                None
            } else {
                Some(
                    event
                        .next_actor
                        .as_slice()
                        .try_into()
                        .map_err(|_| "stored next actor key has wrong length".to_owned())?,
                )
            };
            Ok(EventView {
                seq: event.seq,
                kind: match EventKind::try_from(event.kind) {
                    Ok(EventKind::Genesis) => "GENESIS",
                    Ok(EventKind::Checkpoint) => "CHECKPOINT",
                    Ok(EventKind::Handoff) => "HANDOFF",
                    _ => "UNKNOWN",
                },
                actor: hex::encode(actor),
                lat: event.lat_e6 as f64 / 1e6,
                lon: event.lon_e6 as f64 / 1e6,
                timestamp: event.timestamp,
                note: event.note,
                hash: hex::encode(chain.hash_at(i)),
                next_actor: next_actor.map(hex::encode),
                actor_enrolled: enrolled.contains(&actor),
                next_actor_enrolled: next_actor.map(|key| enrolled.contains(&key)),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use ledger_core::{sign_event, Event, SignedEvent, SigningKey};
    use tower::ServiceExt;

    async fn post_batch(app: &Router, events: Vec<SignedEvent>) -> SyncAck {
        let req = Request::post("/api/v1/events")
            .body(Body::from(SyncBatch { events }.encode_to_vec()))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        SyncAck::decode(res.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }

    #[tokio::test]
    async fn offline_batch_sync_then_public_verify() {
        let farmer = SigningKey::from_bytes(&[1; 32]);
        let driver = SigningKey::from_bytes(&[2; 32]);
        let pid = [7u8; 16];
        let mut store = Store::in_memory();
        let organization_id = store.create_organization("test cooperative").await.unwrap();
        store
            .enroll_device(organization_id, farmer.verifying_key().to_bytes())
            .await
            .unwrap();
        let app = app_with(store);

        let g = sign_event(
            &farmer,
            Event {
                product_id: pid.to_vec(),
                kind: EventKind::Genesis as i32,
                lat_e6: -12_800_000,
                lon_e6: 28_200_000,
                note: "honey".into(),
                ..Default::default()
            },
        );
        let mut local = Chain::from_genesis(g.clone()).unwrap();
        // Device queues two events offline, then syncs everything in one request.
        let h = sign_event(
            &farmer,
            Event {
                product_id: pid.to_vec(),
                seq: 1,
                prev_hash: local.head_hash().to_vec(),
                kind: EventKind::Handoff as i32,
                next_actor: driver.verifying_key().to_bytes().to_vec(),
                ..Default::default()
            },
        );
        local.append(h.clone()).unwrap();
        let cp = sign_event(
            &driver,
            Event {
                product_id: pid.to_vec(),
                seq: 2,
                prev_hash: local.head_hash().to_vec(),
                kind: EventKind::Checkpoint as i32,
                lat_e6: -15_416_700,
                lon_e6: 28_283_300,
                note: "Lusaka warehouse".into(),
                ..Default::default()
            },
        );

        let ack = post_batch(&app, vec![g.clone(), h.clone(), cp.clone()]).await;
        assert_eq!(
            (ack.accepted, ack.duplicates, ack.rejected.len()),
            (3, 0, 0)
        );

        // Ack was lost, device resends: everything is a duplicate, nothing breaks.
        let ack = post_batch(&app, vec![g, h, cp.clone()]).await;
        assert_eq!(
            (ack.accepted, ack.duplicates, ack.rejected.len()),
            (0, 3, 0)
        );

        // Forged event from a stranger is rejected.
        let evil = SigningKey::from_bytes(&[66; 32]);
        let forged = sign_event(
            &evil,
            Event {
                product_id: pid.to_vec(),
                seq: 3,
                prev_hash: local.head_hash().to_vec(),
                kind: EventKind::Checkpoint as i32,
                ..Default::default()
            },
        );
        let ack = post_batch(&app, vec![forged]).await;
        assert_eq!(ack.rejected.len(), 1);

        let res = app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/verify/{}", hex::encode(pid)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v: serde_json::Value =
            serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(v["verified"], true);
        assert_eq!(v["events"].as_array().unwrap().len(), 3);
        assert_eq!(v["events"][2]["note"], "Lusaka warehouse");
        assert_eq!(
            v["current_custodian"],
            hex::encode(driver.verifying_key().to_bytes())
        );
    }

    #[tokio::test]
    async fn unknown_product_and_bad_id() {
        let app = app();
        let res = app
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/verify/{}", "00".repeat(16)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let res = app
            .oneshot(
                Request::get("/api/v1/verify/zzz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn admin_auth_enrollment_and_revocation_rules() {
        let app = app_with_admin_token(Store::in_memory(), Some("test-admin-token".into()));
        let farmer = SigningKey::from_bytes(&[31; 32]);
        let public_key = hex::encode(farmer.verifying_key().to_bytes());
        let unauthorized = app
            .clone()
            .oneshot(
                Request::post("/api/v1/admin/organizations")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"name":"Cooperative"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let create = app
            .clone()
            .oneshot(
                Request::post("/api/v1/admin/organizations")
                    .header(header::AUTHORIZATION, "Bearer test-admin-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"name":"Cooperative"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create.status(), StatusCode::CREATED);
        let organization: serde_json::Value =
            serde_json::from_slice(&create.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let organization_id = organization["id"].as_i64().unwrap();

        let enroll = app
            .clone()
            .oneshot(
                Request::post(format!(
                    "/api/v1/admin/organizations/{organization_id}/devices"
                ))
                .header(header::AUTHORIZATION, "Bearer test-admin-token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(r#"{{"public_key":"{public_key}"}}"#)))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(enroll.status(), StatusCode::CREATED);

        let product_id = [32u8; 16];
        let genesis = sign_event(
            &farmer,
            Event {
                product_id: product_id.to_vec(),
                kind: EventKind::Genesis as i32,
                ..Default::default()
            },
        );
        let genesis_head = Chain::from_genesis(genesis.clone()).unwrap().head_hash();
        assert_eq!(post_batch(&app, vec![genesis.clone()]).await.accepted, 1);
        let revoke = app
            .clone()
            .oneshot(
                Request::delete(format!("/api/v1/admin/devices/{public_key}"))
                    .header(header::AUTHORIZATION, "Bearer test-admin-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(revoke.status(), StatusCode::NO_CONTENT);

        assert_eq!(post_batch(&app, vec![genesis]).await.duplicates, 1);
        let checkpoint = sign_event(
            &farmer,
            Event {
                product_id: product_id.to_vec(),
                seq: 1,
                prev_hash: genesis_head.to_vec(),
                kind: EventKind::Checkpoint as i32,
                ..Default::default()
            },
        );
        assert_eq!(post_batch(&app, vec![checkpoint]).await.rejected.len(), 1);
    }

    #[tokio::test]
    async fn admin_calls_are_disabled_without_token_and_unenrolled_handoffs_are_flagged() {
        let disabled = app();
        let response = disabled
            .oneshot(
                Request::post("/api/v1/admin/organizations")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"name":"No access"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let farmer = SigningKey::from_bytes(&[41; 32]);
        let driver = SigningKey::from_bytes(&[42; 32]);
        let mut store = Store::in_memory();
        let organization_id = store
            .create_organization("Origin cooperative")
            .await
            .unwrap();
        store
            .enroll_device(organization_id, farmer.verifying_key().to_bytes())
            .await
            .unwrap();
        let app = app_with(store);
        let product_id = [43u8; 16];
        let genesis = sign_event(
            &farmer,
            Event {
                product_id: product_id.to_vec(),
                kind: EventKind::Genesis as i32,
                ..Default::default()
            },
        );
        let mut chain = Chain::from_genesis(genesis.clone()).unwrap();
        let handoff = sign_event(
            &farmer,
            Event {
                product_id: product_id.to_vec(),
                seq: 1,
                prev_hash: chain.head_hash().to_vec(),
                kind: EventKind::Handoff as i32,
                next_actor: driver.verifying_key().to_bytes().to_vec(),
                ..Default::default()
            },
        );
        chain.append(handoff.clone()).unwrap();
        let checkpoint = sign_event(
            &driver,
            Event {
                product_id: product_id.to_vec(),
                seq: 2,
                prev_hash: chain.head_hash().to_vec(),
                kind: EventKind::Checkpoint as i32,
                ..Default::default()
            },
        );
        assert_eq!(
            post_batch(&app, vec![genesis, handoff, checkpoint])
                .await
                .accepted,
            3
        );

        let verify = app
            .oneshot(
                Request::get(format!("/api/v1/verify/{}", hex::encode(product_id)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body: serde_json::Value =
            serde_json::from_slice(&verify.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["verified"], true);
        assert_eq!(body["events"][2]["actor_enrolled"], false);
        assert_eq!(body["events"][1]["next_actor_enrolled"], false);
    }
}
