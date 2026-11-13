//! Hand-written prost types mirroring `proto/ledger.proto`.
//! (No protoc needed at build time. Keep in sync with the .proto file.)

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub enum EventKind {
    Unspecified = 0,
    Genesis = 1,
    Checkpoint = 2,
    Handoff = 3,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Event {
    #[prost(bytes = "vec", tag = "1")]
    pub product_id: Vec<u8>,
    #[prost(uint32, tag = "2")]
    pub seq: u32,
    #[prost(bytes = "vec", tag = "3")]
    pub prev_hash: Vec<u8>,
    #[prost(enumeration = "EventKind", tag = "4")]
    pub kind: i32,
    #[prost(bytes = "vec", tag = "5")]
    pub actor_key: Vec<u8>,
    #[prost(sint32, tag = "6")]
    pub lat_e6: i32,
    #[prost(sint32, tag = "7")]
    pub lon_e6: i32,
    #[prost(uint64, tag = "8")]
    pub timestamp: u64,
    #[prost(string, tag = "9")]
    pub note: String,
    #[prost(bytes = "vec", tag = "10")]
    pub next_actor: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct SignedEvent {
    #[prost(bytes = "vec", tag = "1")]
    pub event: Vec<u8>,
    #[prost(bytes = "vec", tag = "2")]
    pub signature: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct SyncBatch {
    #[prost(message, repeated, tag = "1")]
    pub events: Vec<SignedEvent>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Rejection {
    #[prost(uint32, tag = "1")]
    pub index: u32,
    #[prost(string, tag = "2")]
    pub reason: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct SyncAck {
    #[prost(uint32, tag = "1")]
    pub accepted: u32,
    #[prost(uint32, tag = "2")]
    pub duplicates: u32,
    #[prost(message, repeated, tag = "3")]
    pub rejected: Vec<Rejection>,
}
