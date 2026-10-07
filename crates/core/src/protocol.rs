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

/// Upper bound for a single framed control/header message.
pub const MAX_FRAME: usize = 1 << 20;

/// Write a u32 BE length-prefixed JSON message.
pub async fn write_msg<W, T>(w: &mut W, msg: &T) -> anyhow::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
    T: Serialize,
{
    use tokio::io::AsyncWriteExt;
    let body = serde_json::to_vec(msg)?;
    let mut buf = Vec::with_capacity(4 + body.len());
    buf.extend_from_slice(&(body.len() as u32).to_be_bytes());
    buf.extend_from_slice(&body);
    w.write_all(&buf).await?;
    Ok(())
}

/// Read a u32 BE length-prefixed JSON message.
pub async fn read_msg<R, T>(r: &mut R) -> anyhow::Result<T>
where
    R: tokio::io::AsyncRead + Unpin,
    T: serde::de::DeserializeOwned,
{
    use tokio::io::AsyncReadExt;
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    anyhow::ensure!(len <= MAX_FRAME, "frame too large: {len}");
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

/// Encode a UDP datagram: u16 BE service index followed by payload.
pub fn encode_datagram(index: u16, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(2 + payload.len());
    buf.extend_from_slice(&index.to_be_bytes());
    buf.extend_from_slice(payload);
    buf
}

/// Split a datagram into service index and payload.
pub fn decode_datagram(data: &[u8]) -> Option<(u16, &[u8])> {
    if data.len() < 2 {
        return None;
    }
    Some((u16::from_be_bytes([data[0], data[1]]), &data[2..]))
}
