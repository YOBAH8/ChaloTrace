//! ChaloTrace ledger core: compact signed events that form a per-product hash chain.

pub mod chain;
pub mod proto;

pub use chain::{event_hash, generate_key, open_event, sign_event, AppendOutcome, Chain, ChainError, Hash};
pub use ed25519_dalek::SigningKey;
pub use prost::Message;
pub use proto::{Event, EventKind, Rejection, SignedEvent, SyncAck, SyncBatch};

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> SigningKey { SigningKey::from_bytes(&[seed; 32]) }

    fn genesis(k: &SigningKey, pid: [u8; 16]) -> SignedEvent {
        sign_event(k, Event {
            product_id: pid.to_vec(), seq: 0, kind: EventKind::Genesis as i32,
            lat_e6: -12_800_000, lon_e6: 28_200_000, timestamp: 1_760_000_000,
            note: "honey lot 7".into(), ..Default::default()
        })
    }

    fn next(k: &SigningKey, c: &Chain, kind: EventKind, next_actor: Vec<u8>) -> SignedEvent {
        sign_event(k, Event {
            product_id: c.product_id().to_vec(), seq: c.len() as u32,
            prev_hash: c.head_hash().to_vec(), kind: kind as i32,
            lat_e6: -15_416_700, lon_e6: 28_283_300, timestamp: 1_760_086_400,
            next_actor, ..Default::default()
        })
    }

    #[test]
    fn happy_path_with_handoff() {
        let (farmer, driver) = (key(1), key(2));
        let mut c = Chain::from_genesis(genesis(&farmer, [9; 16])).unwrap();
        let h = next(&farmer, &c, EventKind::Handoff, driver.verifying_key().to_bytes().to_vec());
        assert_eq!(c.append(h).unwrap(), AppendOutcome::Appended);
        let cp = next(&driver, &c, EventKind::Checkpoint, vec![]);
        assert_eq!(c.append(cp).unwrap(), AppendOutcome::Appended);
        // Independent replay from scratch agrees.
        let replay = Chain::verify(c.events()).unwrap();
        assert_eq!(replay.head_hash(), c.head_hash());
        assert_eq!(replay.custodian(), driver.verifying_key().to_bytes());
    }

    #[test]
    fn former_custodian_cannot_write_after_handoff() {
        let (farmer, driver) = (key(1), key(2));
        let mut c = Chain::from_genesis(genesis(&farmer, [9; 16])).unwrap();
        let h = next(&farmer, &c, EventKind::Handoff, driver.verifying_key().to_bytes().to_vec());
        c.append(h).unwrap();
        let bad = next(&farmer, &c, EventKind::Checkpoint, vec![]);
        assert_eq!(c.append(bad), Err(ChainError::NotCustodian));
    }

    #[test]
    fn stranger_cannot_write() {
        let mut c = Chain::from_genesis(genesis(&key(1), [9; 16])).unwrap();
        let bad = next(&key(7), &c, EventKind::Checkpoint, vec![]);
        assert_eq!(c.append(bad), Err(ChainError::NotCustodian));
    }

    #[test]
    fn tampering_is_detected() {
        let k = key(1);
        let mut c = Chain::from_genesis(genesis(&k, [9; 16])).unwrap();
        let mut e = next(&k, &c, EventKind::Checkpoint, vec![]);
        e.event[5] ^= 0x01;
        assert!(matches!(c.append(e), Err(ChainError::BadSignature) | Err(ChainError::Malformed(_))));
    }

    #[test]
    fn retransmit_is_duplicate_and_fork_is_conflict() {
        let k = key(1);
        let mut c = Chain::from_genesis(genesis(&k, [9; 16])).unwrap();
        let e = next(&k, &c, EventKind::Checkpoint, vec![]);
        assert_eq!(c.append(e.clone()).unwrap(), AppendOutcome::Appended);
        assert_eq!(c.append(e).unwrap(), AppendOutcome::Duplicate);
        let mut fork = Event::decode(c.events()[1].event.as_slice()).unwrap();
        fork.lat_e6 += 1000;
        assert_eq!(c.append(sign_event(&k, fork)), Err(ChainError::Conflict));
    }

    #[test]
    fn gaps_and_wrong_prev_hash_rejected() {
        let k = key(1);
        let mut c = Chain::from_genesis(genesis(&k, [9; 16])).unwrap();
        let mut ev = Event::decode(next(&k, &c, EventKind::Checkpoint, vec![]).event.as_slice()).unwrap();
        ev.seq = 5;
        assert_eq!(c.append(sign_event(&k, ev.clone())), Err(ChainError::SeqMismatch { expected: 1, got: 5 }));
        ev.seq = 1;
        ev.prev_hash = vec![0; 32];
        assert_eq!(c.append(sign_event(&k, ev)), Err(ChainError::PrevHashMismatch));
    }

    #[test]
    fn rejects_bad_coordinates_and_second_genesis() {
        let k = key(1);
        let mut c = Chain::from_genesis(genesis(&k, [9; 16])).unwrap();
        let mut ev = Event::decode(next(&k, &c, EventKind::Checkpoint, vec![]).event.as_slice()).unwrap();
        ev.lat_e6 = 91_000_000;
        assert_eq!(c.append(sign_event(&k, ev)), Err(ChainError::BadCoordinates));
        let mut g = Event::decode(genesis(&k, [9; 16]).event.as_slice()).unwrap();
        g.seq = 1;
        assert!(c.append(sign_event(&k, g)).is_err());
    }

    /// Prints real wire sizes. Run: cargo test -p ledger-core wire_size -- --nocapture
    #[test]
    fn wire_size_report() {
        let k = key(1);
        let c = Chain::from_genesis(genesis(&k, [9; 16])).unwrap();
        let cp = next(&k, &c, EventKind::Checkpoint, vec![]);
        let one = SyncBatch { events: vec![cp.clone()] }.encode_to_vec().len();
        let ten = SyncBatch { events: vec![cp; 10] }.encode_to_vec().len();
        println!("protobuf: 1 checkpoint = {one} bytes, batch of 10 = {ten} bytes");
        assert!(one < 200);
    }
}
