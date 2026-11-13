//! PostgreSQL integration tests for persistence, duplicate sync, tamper detection, and immutability.

use ledger_core::{sign_event, AppendOutcome, Chain, Event, EventKind, SignedEvent, SigningKey};
use ledger_server::Store;
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use std::sync::atomic::{AtomicU64, Ordering};

static SCHEMA_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDatabase {
    admin: PgPool,
    schema: String,
    scoped_url: String,
}

impl TestDatabase {
    async fn create() -> Option<Self> {
        let base_url = match std::env::var("TEST_DATABASE_URL") {
            Ok(url) => url,
            Err(_) => {
                eprintln!("Skipping PostgreSQL integration test: TEST_DATABASE_URL is not set");
                return None;
            }
        };
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&base_url)
            .await
            .expect("connect to TEST_DATABASE_URL");
        let schema = format!(
            "chalotrace_test_{}_{}",
            std::process::id(),
            SCHEMA_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .expect("create isolated test schema");
        let separator = if base_url.contains('?') { '&' } else { '?' };
        let scoped_url = format!("{base_url}{separator}options%5Bsearch_path%5D={schema}");

        Some(Self {
            admin,
            schema,
            scoped_url,
        })
    }

    async fn cleanup(self) {
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .expect("drop isolated test schema");
        self.admin.close().await;
    }

    async fn pool(&self) -> PgPool {
        PgPoolOptions::new()
            .max_connections(2)
            .connect(&self.scoped_url)
            .await
            .expect("connect to isolated test schema")
    }

    async fn store(&self) -> Store {
        let mut store = Store::connect(&self.scoped_url)
            .await
            .expect("connect isolated store");
        let organization_id = store
            .create_organization("Integration-test cooperative")
            .await
            .expect("create test organization");
        store
            .enroll_device(
                organization_id,
                SigningKey::from_bytes(&[11; 32]).verifying_key().to_bytes(),
            )
            .await
            .expect("enroll genesis test key");
        store
    }
}

fn events(product_id: [u8; 16]) -> Vec<SignedEvent> {
    let farmer = SigningKey::from_bytes(&[11; 32]);
    let genesis = sign_event(
        &farmer,
        Event {
            product_id: product_id.to_vec(),
            kind: EventKind::Genesis as i32,
            note: "test batch".into(),
            ..Default::default()
        },
    );
    let chain = Chain::from_genesis(genesis.clone()).expect("valid genesis fixture");
    let checkpoint = sign_event(
        &farmer,
        Event {
            product_id: product_id.to_vec(),
            seq: 1,
            prev_hash: chain.head_hash().to_vec(),
            kind: EventKind::Checkpoint as i32,
            note: "test checkpoint".into(),
            ..Default::default()
        },
    );
    vec![genesis, checkpoint]
}

#[tokio::test]
async fn events_survive_connecting_a_new_store() {
    let Some(db) = TestDatabase::create().await else {
        return;
    };
    let product_id = [21; 16];
    {
        let mut store = db.store().await;
        for event in events(product_id) {
            assert_eq!(store.apply(event).await.unwrap(), AppendOutcome::Appended);
        }
    }

    let reloaded = Store::connect(&db.scoped_url)
        .await
        .expect("connect reloaded store");
    let chain = reloaded
        .get(&product_id)
        .expect("product loaded from PostgreSQL");
    assert_eq!(chain.len(), 2);
    assert_eq!(chain.events().len(), 2);
    drop(reloaded);
    db.cleanup().await;
}

#[tokio::test]
async fn retransmitted_batch_events_are_duplicates() {
    let Some(db) = TestDatabase::create().await else {
        return;
    };
    let mut store = db.store().await;
    let batch = events([22; 16]);

    for event in &batch {
        assert_eq!(
            store.apply(event.clone()).await.unwrap(),
            AppendOutcome::Appended
        );
    }
    for event in batch {
        assert_eq!(store.apply(event).await.unwrap(), AppendOutcome::Duplicate);
    }
    assert_eq!(store.get(&[22; 16]).expect("stored product").len(), 2);
    drop(store);
    db.cleanup().await;
}

#[tokio::test]
async fn tampered_event_row_fails_startup_verification() {
    let Some(db) = TestDatabase::create().await else {
        return;
    };
    let product_id = [23u8; 16];
    let mut store = db.store().await;
    store.apply(events(product_id).remove(0)).await.unwrap();
    drop(store);

    let pool = db.pool().await;
    sqlx::query("ALTER TABLE events DISABLE TRIGGER events_no_update")
        .execute(&pool)
        .await
        .expect("disable append-only trigger for tampering fixture");
    let row = sqlx::query("SELECT event_bytes FROM events WHERE product_id = $1 AND seq = 0")
        .bind(product_id.as_slice())
        .fetch_one(&pool)
        .await
        .expect("load genesis bytes");
    let mut event_bytes: Vec<u8> = row.get("event_bytes");
    event_bytes[0] ^= 0x01;
    sqlx::query("UPDATE events SET event_bytes = $1 WHERE product_id = $2 AND seq = 0")
        .bind(event_bytes)
        .bind(product_id.as_slice())
        .execute(&pool)
        .await
        .expect("tamper with genesis bytes");
    pool.close().await;

    let error = match Store::connect(&db.scoped_url).await {
        Ok(_) => panic!("tampered event unexpectedly passed startup verification"),
        Err(error) => error,
    };
    assert!(
        error.contains("FAILED verification"),
        "unexpected error: {error}"
    );
    db.cleanup().await;
}

#[tokio::test]
async fn append_only_trigger_rejects_update_and_delete() {
    let Some(db) = TestDatabase::create().await else {
        return;
    };
    let product_id = [24u8; 16];
    let mut store = db.store().await;
    store.apply(events(product_id).remove(0)).await.unwrap();
    drop(store);

    let pool = db.pool().await;
    let update = sqlx::query("UPDATE events SET event_bytes = event_bytes WHERE product_id = $1")
        .bind(product_id.as_slice())
        .execute(&pool)
        .await;
    assert!(
        update
            .unwrap_err()
            .to_string()
            .contains("events are append-only"),
        "UPDATE should be blocked by the append-only trigger"
    );
    let delete = sqlx::query("DELETE FROM events WHERE product_id = $1")
        .bind(product_id.as_slice())
        .execute(&pool)
        .await;
    assert!(
        delete
            .unwrap_err()
            .to_string()
            .contains("events are append-only"),
        "DELETE should be blocked by the append-only trigger"
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE product_id = $1")
        .bind(product_id.as_slice())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    pool.close().await;
    db.cleanup().await;
}

#[tokio::test]
async fn enrollment_is_persisted_and_revocation_only_blocks_new_events() {
    let Some(db) = TestDatabase::create().await else {
        return;
    };
    let product_id = [25u8; 16];
    let mut store = Store::connect(&db.scoped_url).await.expect("connect store");
    let organization_id = store
        .create_organization("Revocation-test cooperative")
        .await
        .unwrap();
    let signer = SigningKey::from_bytes(&[25; 32]);
    let public_key = signer.verifying_key().to_bytes();
    store
        .enroll_device(organization_id, public_key)
        .await
        .unwrap();
    let genesis = sign_event(
        &signer,
        Event {
            product_id: product_id.to_vec(),
            kind: EventKind::Genesis as i32,
            ..Default::default()
        },
    );
    let chain = Chain::from_genesis(genesis.clone()).unwrap();
    assert_eq!(store.apply(genesis).await.unwrap(), AppendOutcome::Appended);
    assert!(store.revoke_device(public_key).await.unwrap());

    let checkpoint = sign_event(
        &signer,
        Event {
            product_id: product_id.to_vec(),
            seq: 1,
            prev_hash: chain.head_hash().to_vec(),
            kind: EventKind::Checkpoint as i32,
            ..Default::default()
        },
    );
    assert!(store
        .apply(checkpoint)
        .await
        .unwrap_err()
        .contains("revoked"));
    drop(store);

    let reloaded = Store::connect(&db.scoped_url)
        .await
        .expect("historical chain remains valid after revocation");
    assert_eq!(reloaded.get(&product_id).unwrap().len(), 1);
    drop(reloaded);
    db.cleanup().await;
}
