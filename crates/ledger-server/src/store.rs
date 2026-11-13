//! STORAGE LAYER: keeps every product chain in memory for fast reads and, when a database URL is
//! given, also saves each accepted event to PostgreSQL.
//!
//! Safety properties:
//!  - Write order is: validate -> save to database -> update memory. If the database save fails,
//!    memory is untouched, so memory never claims something the database does not have.
//!  - On startup every stored chain is re-verified from its signatures. If any row was altered
//!    in the database, the server refuses to start and says which product is damaged.
//!  - This design assumes ONE server process per database (the in-memory copy is the live view).

use ledger_core::{event_hash, open_event, AppendOutcome, Chain, EventKind, SignedEvent};
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use std::collections::HashMap;

pub struct Store {
    chains: HashMap<[u8; 16], Chain>,
    pool: Option<PgPool>,
}

impl Store {
    /// No database: data is lost when the server stops (used by tests and quick experiments).
    pub fn in_memory() -> Self {
        Store { chains: HashMap::new(), pool: None }
    }

    /// Connect to PostgreSQL, create the tables if needed, and load + verify all saved chains.
    pub async fn connect(database_url: &str) -> Result<Self, String> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await
            .map_err(|e| format!("cannot connect to PostgreSQL: {e}"))?;
        sqlx::migrate!("../../migrations")
            .run(&pool)
            .await
            .map_err(|e| format!("database setup (migrations) failed: {e}"))?;
        let chains = load_all(&pool).await?;
        Ok(Store { chains, pool: Some(pool) })
    }

    pub fn get(&self, product_id: &[u8; 16]) -> Option<&Chain> {
        self.chains.get(product_id)
    }

    pub fn product_count(&self) -> usize {
        self.chains.len()
    }

    /// Validate one signed event and, if it is new and valid, save and apply it.
    pub async fn apply(&mut self, se: SignedEvent) -> Result<AppendOutcome, String> {
        let ev = open_event(&se).map_err(|e| e.to_string())?;
        let pid: [u8; 16] = ev.product_id.as_slice().try_into().map_err(|_| "bad product_id".to_string())?;
        let hash = event_hash(&se);

        if ev.kind == EventKind::Genesis as i32 {
            if let Some(existing) = self.chains.get(&pid) {
                // Same genesis again = retransmit (Duplicate); a different one = Conflict error.
                return existing.check(&se).map_err(|e| e.to_string());
            }
            let chain = Chain::from_genesis(se.clone()).map_err(|e| e.to_string())?;
            if let Some(pool) = &self.pool {
                persist(pool, &pid, &se, &hash, ev.seq, &ev.actor_key, ev.kind, ev.lat_e6, ev.lon_e6, ev.timestamp, true).await?;
            }
            self.chains.insert(pid, chain);
            return Ok(AppendOutcome::Appended);
        }

        let chain = self.chains.get(&pid).ok_or("unknown product (genesis not received yet)")?;
        if chain.check(&se).map_err(|e| e.to_string())? == AppendOutcome::Duplicate {
            return Ok(AppendOutcome::Duplicate);
        }
        if let Some(pool) = &self.pool {
            // After a HANDOFF the next actor is the custodian; otherwise it stays the signer.
            let custodian_after = if ev.kind == EventKind::Handoff as i32 { &ev.next_actor } else { &ev.actor_key };
            persist(pool, &pid, &se, &hash, ev.seq, custodian_after, ev.kind, ev.lat_e6, ev.lon_e6, ev.timestamp, false).await?;
        }
        self.chains.get_mut(&pid).unwrap().append(se).map_err(|e| e.to_string())
    }
}

/// Save one event (and create or update its product row) in a single database transaction.
#[allow(clippy::too_many_arguments)]
async fn persist(
    pool: &PgPool, pid: &[u8; 16], se: &SignedEvent, hash: &[u8; 32], seq: u32,
    custodian_after: &[u8], kind: i32, lat_e6: i32, lon_e6: i32, device_ts: u64, is_genesis: bool,
) -> Result<(), String> {
    let db = |e: sqlx::Error| format!("database write failed: {e}");
    let mut tx = pool.begin().await.map_err(db)?;
    if is_genesis {
        sqlx::query("INSERT INTO products (product_id, custodian, head_hash, event_count) VALUES ($1, $2, $3, 1)")
            .bind(&pid[..]).bind(custodian_after).bind(&hash[..])
            .execute(&mut *tx).await.map_err(db)?;
    } else {
        sqlx::query("UPDATE products SET custodian = $2, head_hash = $3, event_count = $4 WHERE product_id = $1")
            .bind(&pid[..]).bind(custodian_after).bind(&hash[..]).bind(seq as i32 + 1)
            .execute(&mut *tx).await.map_err(db)?;
    }
    // Actor key is read back out of the event so the indexed column always matches the signed bytes.
    let actor = open_event(se).map_err(|e| e.to_string())?.actor_key;
    sqlx::query(
        "INSERT INTO events (product_id, seq, event_hash, event_bytes, signature, actor_key, kind, lat_e6, lon_e6, device_ts)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(&pid[..]).bind(seq as i32).bind(&hash[..]).bind(&se.event[..]).bind(&se.signature[..])
    .bind(&actor[..]).bind(kind as i16).bind(lat_e6).bind(lon_e6).bind(device_ts as i64)
    .execute(&mut *tx).await.map_err(db)?;
    tx.commit().await.map_err(db)
}

/// Read every saved event and rebuild + re-verify each chain from its signatures.
async fn load_all(pool: &PgPool) -> Result<HashMap<[u8; 16], Chain>, String> {
    let rows = sqlx::query("SELECT product_id, event_bytes, signature FROM events ORDER BY product_id, seq")
        .fetch_all(pool)
        .await
        .map_err(|e| format!("cannot read saved events: {e}"))?;
    let mut grouped: HashMap<[u8; 16], Vec<SignedEvent>> = HashMap::new();
    for row in rows {
        let pid: Vec<u8> = row.get("product_id");
        let pid: [u8; 16] = pid.try_into().map_err(|_| "saved product_id has wrong length".to_string())?;
        grouped.entry(pid).or_default().push(SignedEvent { event: row.get("event_bytes"), signature: row.get("signature") });
    }
    let mut chains = HashMap::new();
    for (pid, events) in grouped {
        let chain = Chain::verify(&events)
            .map_err(|e| format!("saved chain for product {} FAILED verification ({e}); the database may have been tampered with", hex(&pid)))?;
        chains.insert(pid, chain);
    }
    Ok(chains)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
