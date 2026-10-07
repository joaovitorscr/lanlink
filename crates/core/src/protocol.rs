//! Wire protocol over a lanlink QUIC connection.
//!
//! Control: the FIRST bidirectional stream opened by the dialer is the control stream.
//! Messages are length-prefixed (u32 BE) JSON `ControlMsg`.
//! Data: every later bi stream begins with a `StreamHeader` (length-prefixed JSON), then raw bytes.
//! UDP: QUIC datagrams, first byte(s) = service index (u16 BE) then payload.

use crate::Service;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlMsg {
    Hello { name: Option<String> },
    Services(Vec<Service>),
    Ping(u64),
    Pong(u64),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamHeader {
    pub service: String,
}
