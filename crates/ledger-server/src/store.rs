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
use sqlx::{postgres::PgPoolOptions, PgPool, Postgres, QueryBuilder, Row};
use std::collections::{HashMap, HashSet};

pub struct Store {
    chains: HashMap<[u8; 16], Chain>,
    pool: Option<PgPool>,
    organizations: HashMap<i64, String>,
    devices: HashMap<[u8; 32], Device>,
    next_organization_id: i64,
}

#[derive(Clone, Copy)]
struct Device {
    revoked: bool,
}

impl Store {
    /// No database: data is lost when the server stops (used by tests and quick experiments).
    pub fn in_memory() -> Self {
        Store {
            chains: HashMap::new(),
            pool: None,
            organizations: HashMap::new(),
            devices: HashMap::new(),
            next_organization_id: 1,
        }
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
        Ok(Store {
            chains,
            pool: Some(pool),
            organizations: HashMap::new(),
            devices: HashMap::new(),
            next_organization_id: 1,
        })
    }

    pub fn get(&self, product_id: &[u8; 16]) -> Option<&Chain> {
        self.chains.get(product_id)
    }

    pub fn product_count(&self) -> usize {
        self.chains.len()
    }

    pub async fn create_organization(&mut self, name: &str) -> Result<i64, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("organization name must not be empty".into());
        }
        if let Some(pool) = &self.pool {
            return sqlx::query_scalar("INSERT INTO organizations (name) VALUES ($1) RETURNING id")
                .bind(name)
                .fetch_one(pool)
                .await
                .map_err(|e| format!("cannot create organization: {e}"));
        }
        if self.organizations.values().any(|existing| existing == name) {
            return Err("organization name already exists".into());
        }
        let id = self.next_organization_id;
        self.next_organization_id += 1;
        self.organizations.insert(id, name.to_owned());
        Ok(id)
    }

    pub async fn enroll_device(
        &mut self,
        organization_id: i64,
        public_key: [u8; 32],
    ) -> Result<(), String> {
        if let Some(pool) = &self.pool {
            return sqlx::query(
                "INSERT INTO devices (public_key, organization_id) VALUES ($1, $2)",
            )
            .bind(public_key.as_slice())
            .bind(organization_id)
            .execute(pool)
            .await
            .map(|_| ())
            .map_err(|e| format!("cannot enroll device: {e}"));
        }
        if !self.organizations.contains_key(&organization_id) {
            return Err("unknown organization".into());
        }
        if self.devices.contains_key(&public_key) {
            return Err("device key is already enrolled".into());
        }
        self.devices.insert(public_key, Device { revoked: false });
        Ok(())
    }

    pub async fn revoke_device(&mut self, public_key: [u8; 32]) -> Result<bool, String> {
        if let Some(pool) = &self.pool {
            let result = sqlx::query(
                "UPDATE devices SET revoked_at = COALESCE(revoked_at, now()) WHERE public_key = $1",
            )
            .bind(public_key.as_slice())
            .execute(pool)
            .await
            .map_err(|e| format!("cannot revoke device: {e}"))?;
            return Ok(result.rows_affected() != 0);
        }
        match self.devices.get_mut(&public_key) {
            Some(device) => {
                device.revoked = true;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub async fn registered_device_keys(
        &self,
        keys: &HashSet<[u8; 32]>,
    ) -> Result<HashSet<[u8; 32]>, String> {
        if keys.is_empty() {
            return Ok(HashSet::new());
        }
        if let Some(pool) = &self.pool {
            let mut query = QueryBuilder::<Postgres>::new(
                "SELECT public_key FROM devices WHERE public_key IN (",
            );
            let mut separated = query.separated(", ");
            for key in keys {
                separated.push_bind(key.as_slice());
            }
            separated.push_unseparated(")");
            let rows = query
                .build()
                .fetch_all(pool)
                .await
                .map_err(|e| format!("cannot read enrolled devices: {e}"))?;
            return rows
                .into_iter()
                .map(|row| {
                    let bytes: Vec<u8> = row.get("public_key");
                    bytes
                        .try_into()
                        .map_err(|_| "saved device public key has wrong length".to_string())
                })
                .collect();
        }
        Ok(keys
            .iter()
            .filter(|key| self.devices.contains_key(*key))
            .copied()
            .collect())
    }

    /// Validate one signed event and, if it is new and valid, save and apply it.
    pub async fn apply(&mut self, se: SignedEvent) -> Result<AppendOutcome, String> {
        let ev = open_event(&se).map_err(|e| e.to_string())?;
        let pid: [u8; 16] = ev
            .product_id
            .as_slice()
            .try_into()
            .map_err(|_| "bad product_id".to_string())?;
        let hash = event_hash(&se);

        if ev.kind == EventKind::Genesis as i32 {
            if let Some(existing) = self.chains.get(&pid) {
                // Same genesis again = retransmit (Duplicate); a different one = Conflict error.
                return existing.check(&se).map_err(|e| e.to_string());
            }
            let chain = Chain::from_genesis(se.clone()).map_err(|e| e.to_string())?;
            if let Some(pool) = &self.pool {
                persist(
                    pool,
                    &pid,
                    &se,
                    &hash,
                    ev.seq,
                    &ev.actor_key,
                    ev.kind,
                    ev.lat_e6,
                    ev.lon_e6,
                    ev.timestamp,
                    true,
                    &ev.actor_key,
                )
                .await?;
            } else {
                self.validate_device_use(&ev.actor_key, true)?;
            }
            self.chains.insert(pid, chain);
            return Ok(AppendOutcome::Appended);
        }

        let chain = self
            .chains
            .get(&pid)
            .ok_or("unknown product (genesis not received yet)")?;
        if chain.check(&se).map_err(|e| e.to_string())? == AppendOutcome::Duplicate {
            return Ok(AppendOutcome::Duplicate);
        }
        if let Some(pool) = &self.pool {
            // After a HANDOFF the next actor is the custodian; otherwise it stays the signer.
            let custodian_after = if ev.kind == EventKind::Handoff as i32 {
                &ev.next_actor
            } else {
                &ev.actor_key
            };
            persist(
                pool,
                &pid,
                &se,
                &hash,
                ev.seq,
                custodian_after,
                ev.kind,
                ev.lat_e6,
                ev.lon_e6,
                ev.timestamp,
                false,
                &ev.actor_key,
            )
            .await?;
        } else {
            self.validate_device_use(&ev.actor_key, false)?;
        }
        self.chains
            .get_mut(&pid)
            .unwrap()
            .append(se)
            .map_err(|e| e.to_string())
    }

    fn validate_device_use(&self, actor_key: &[u8], genesis: bool) -> Result<(), String> {
        let key: [u8; 32] = actor_key
            .try_into()
            .map_err(|_| "actor_key must be 32 bytes".to_string())?;
        match self.devices.get(&key) {
            Some(device) if device.revoked => Err("device key has been revoked".into()),
            Some(_) => Ok(()),
            None if genesis => Err("GENESIS signer is not an enrolled device".into()),
            None => Ok(()),
        }
    }
}

/// Save one event (and create or update its product row) in a single database transaction.
#[allow(clippy::too_many_arguments)]
async fn persist(
    pool: &PgPool,
    pid: &[u8; 16],
    se: &SignedEvent,
    hash: &[u8; 32],
    seq: u32,
    custodian_after: &[u8],
    kind: i32,
    lat_e6: i32,
    lon_e6: i32,
    device_ts: u64,
    is_genesis: bool,
    actor_key: &[u8],
) -> Result<(), String> {
    let db = |e: sqlx::Error| format!("database write failed: {e}");
    let mut tx = pool.begin().await.map_err(db)?;
    let active = sqlx::query(
        "SELECT revoked_at IS NULL AS active FROM devices WHERE public_key = $1 FOR SHARE",
    )
    .bind(actor_key)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db)?
    .map(|row| row.get::<bool, _>("active"));
    match (is_genesis, active) {
        (true, Some(true)) | (false, _) if active != Some(false) => {}
        (true, _) => return Err("GENESIS signer is not an enrolled device".into()),
        (false, Some(false)) => return Err("device key has been revoked".into()),
        (false, _) => {}
    }
    if is_genesis {
        sqlx::query("INSERT INTO products (product_id, custodian, head_hash, event_count) VALUES ($1, $2, $3, 1)")
            .bind(&pid[..]).bind(custodian_after).bind(&hash[..])
            .execute(&mut *tx).await.map_err(db)?;
    } else {
        sqlx::query("UPDATE products SET custodian = $2, head_hash = $3, event_count = $4 WHERE product_id = $1")
            .bind(&pid[..]).bind(custodian_after).bind(&hash[..]).bind(seq as i32 + 1)
            .execute(&mut *tx).await.map_err(db)?;
    }
    sqlx::query(
        "INSERT INTO events (product_id, seq, event_hash, event_bytes, signature, actor_key, kind, lat_e6, lon_e6, device_ts)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(&pid[..]).bind(seq as i32).bind(&hash[..]).bind(&se.event[..]).bind(&se.signature[..])
    .bind(actor_key).bind(kind as i16).bind(lat_e6).bind(lon_e6).bind(device_ts as i64)
    .execute(&mut *tx).await.map_err(db)?;
    tx.commit().await.map_err(db)
}

/// Read every saved event and rebuild + re-verify each chain from its signatures.
async fn load_all(pool: &PgPool) -> Result<HashMap<[u8; 16], Chain>, String> {
    let rows = sqlx::query(
        "SELECT product_id, event_bytes, signature FROM events ORDER BY product_id, seq",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("cannot read saved events: {e}"))?;
    let mut grouped: HashMap<[u8; 16], Vec<SignedEvent>> = HashMap::new();
    for row in rows {
        let pid: Vec<u8> = row.get("product_id");
        let pid: [u8; 16] = pid
            .try_into()
            .map_err(|_| "saved product_id has wrong length".to_string())?;
        grouped.entry(pid).or_default().push(SignedEvent {
            event: row.get("event_bytes"),
            signature: row.get("signature"),
        });
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
