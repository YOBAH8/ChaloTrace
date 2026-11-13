//! Signing, hashing and chain validation.
//!
//! Rules enforced for every product chain:
//! 1. Event 0 is a GENESIS; no other event may be GENESIS.
//! 2. `seq` increases by exactly 1; `prev_hash` is the BLAKE3 hash of the previous SignedEvent.
//! 3. Every event is signed (Ed25519) by the *current custodian*.
//! 4. A HANDOFF names `next_actor`, who becomes custodian for the following events.

use crate::proto::{Event, EventKind, SignedEvent};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use prost::Message;

pub type Hash = [u8; 32];
pub const MAX_NOTE_BYTES: usize = 64;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ChainError {
    #[error("malformed event: {0}")]
    Malformed(&'static str),
    #[error("bad signature")]
    BadSignature,
    #[error("first event must be GENESIS")]
    NotGenesis,
    #[error("GENESIS is only allowed at seq 0")]
    UnexpectedGenesis,
    #[error("sequence gap: expected {expected}, got {got}")]
    SeqMismatch { expected: u32, got: u32 },
    #[error("prev_hash does not match chain head")]
    PrevHashMismatch,
    #[error("signer is not the current custodian")]
    NotCustodian,
    #[error("product_id does not match this chain")]
    WrongProduct,
    #[error("coordinates out of range")]
    BadCoordinates,
    #[error("a different event already exists at this seq")]
    Conflict,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AppendOutcome {
    Appended,
    /// Identical event already stored: safe for the device to drop from its queue.
    Duplicate,
}

pub fn generate_key() -> SigningKey {
    SigningKey::generate(&mut rand_core::OsRng)
}

/// Hash that links the next event to this one.
pub fn event_hash(se: &SignedEvent) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(&se.event);
    h.update(&se.signature);
    *h.finalize().as_bytes()
}

/// Encode and sign an event. `actor_key` is filled in from the key.
pub fn sign_event(key: &SigningKey, mut ev: Event) -> SignedEvent {
    ev.actor_key = key.verifying_key().to_bytes().to_vec();
    let bytes = ev.encode_to_vec();
    let sig = key.sign(&bytes);
    SignedEvent { event: bytes, signature: sig.to_bytes().to_vec() }
}

/// Decode an event and verify its signature against its own `actor_key`.
pub fn open_event(se: &SignedEvent) -> Result<Event, ChainError> {
    let ev = Event::decode(se.event.as_slice()).map_err(|_| ChainError::Malformed("undecodable event"))?;
    let key_bytes: [u8; 32] = ev.actor_key.as_slice().try_into().map_err(|_| ChainError::Malformed("actor_key must be 32 bytes"))?;
    let sig_bytes: [u8; 64] = se.signature.as_slice().try_into().map_err(|_| ChainError::Malformed("signature must be 64 bytes"))?;
    let vk = VerifyingKey::from_bytes(&key_bytes).map_err(|_| ChainError::Malformed("invalid public key"))?;
    vk.verify_strict(&se.event, &Signature::from_bytes(&sig_bytes)).map_err(|_| ChainError::BadSignature)?;
    check_shape(&ev)?;
    Ok(ev)
}

fn check_shape(ev: &Event) -> Result<(), ChainError> {
    if ev.product_id.len() != 16 {
        return Err(ChainError::Malformed("product_id must be 16 bytes"));
    }
    if ev.note.len() > MAX_NOTE_BYTES {
        return Err(ChainError::Malformed("note too long"));
    }
    if ev.lat_e6.unsigned_abs() > 90_000_000 || ev.lon_e6.unsigned_abs() > 180_000_000 {
        return Err(ChainError::BadCoordinates);
    }
    let kind = EventKind::try_from(ev.kind).unwrap_or(EventKind::Unspecified);
    match kind {
        EventKind::Unspecified => return Err(ChainError::Malformed("unknown kind")),
        EventKind::Handoff => {
            if ev.next_actor.len() != 32 {
                return Err(ChainError::Malformed("HANDOFF needs 32-byte next_actor"));
            }
        }
        _ => {
            if !ev.next_actor.is_empty() {
                return Err(ChainError::Malformed("next_actor only allowed on HANDOFF"));
            }
        }
    }
    if kind == EventKind::Genesis {
        if ev.seq != 0 || !ev.prev_hash.is_empty() {
            return Err(ChainError::Malformed("GENESIS must have seq 0 and empty prev_hash"));
        }
    } else if ev.prev_hash.len() != 32 {
        return Err(ChainError::Malformed("prev_hash must be 32 bytes"));
    }
    Ok(())
}

/// A validated chain for one product.
#[derive(Debug, Clone)]
pub struct Chain {
    product_id: [u8; 16],
    events: Vec<SignedEvent>,
    hashes: Vec<Hash>,
    custodian: [u8; 32],
}

impl Chain {
    /// Start a chain from a genesis event.
    pub fn from_genesis(se: SignedEvent) -> Result<Self, ChainError> {
        let ev = open_event(&se)?;
        if ev.kind != EventKind::Genesis as i32 {
            return Err(ChainError::NotGenesis);
        }
        let hash = event_hash(&se);
        Ok(Chain {
            product_id: ev.product_id.as_slice().try_into().unwrap(),
            custodian: ev.actor_key.as_slice().try_into().unwrap(),
            events: vec![se],
            hashes: vec![hash],
        })
    }

    /// Validate a full list of events from scratch (what an auditor or buyer does).
    pub fn verify(events: &[SignedEvent]) -> Result<Self, ChainError> {
        let mut it = events.iter();
        let first = it.next().ok_or(ChainError::NotGenesis)?;
        let mut chain = Chain::from_genesis(first.clone())?;
        for se in it {
            chain.append(se.clone())?;
        }
        Ok(chain)
    }

    pub fn append(&mut self, se: SignedEvent) -> Result<AppendOutcome, ChainError> {
        let ev = open_event(&se)?;
        if ev.product_id != self.product_id {
            return Err(ChainError::WrongProduct);
        }
        let seq = ev.seq as usize;
        if seq < self.events.len() {
            // Retransmission after a dropped ack is normal on 2G: accept identical, reject conflicting.
            return if self.hashes[seq] == event_hash(&se) { Ok(AppendOutcome::Duplicate) } else { Err(ChainError::Conflict) };
        }
        if ev.kind == EventKind::Genesis as i32 {
            return Err(ChainError::UnexpectedGenesis);
        }
        if seq != self.events.len() {
            return Err(ChainError::SeqMismatch { expected: self.events.len() as u32, got: ev.seq });
        }
        if ev.prev_hash.as_slice() != self.head_hash().as_slice() {
            return Err(ChainError::PrevHashMismatch);
        }
        if ev.actor_key.as_slice() != self.custodian.as_slice() {
            return Err(ChainError::NotCustodian);
        }
        if ev.kind == EventKind::Handoff as i32 {
            self.custodian = ev.next_actor.as_slice().try_into().unwrap();
        }
        self.hashes.push(event_hash(&se));
        self.events.push(se);
        Ok(AppendOutcome::Appended)
    }

    pub fn product_id(&self) -> [u8; 16] { self.product_id }
    pub fn len(&self) -> usize { self.events.len() }
    pub fn is_empty(&self) -> bool { self.events.is_empty() }
    pub fn custodian(&self) -> [u8; 32] { self.custodian }
    pub fn events(&self) -> &[SignedEvent] { &self.events }
    pub fn head_hash(&self) -> Hash { *self.hashes.last().expect("chain is never empty") }
    pub fn hash_at(&self, i: usize) -> Hash { self.hashes[i] }
}
