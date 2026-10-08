//! Wire protocol over a lanlink QUIC connection.
//!
//! Control: the FIRST bidirectional stream opened by the dialer is the control stream.
//! Messages are length-prefixed (u32 BE) JSON `ControlMsg`.
//! Data: every later bi stream begins with a `StreamHeader` (length-prefixed JSON), then raw bytes.
//! UDP: QUIC datagrams, each starting with a [`DatagramHeader`] (service index, flow id,
//! message id, fragment index/count) followed by payload. Payloads larger than the connection's
//! datagram limit are split into fragments, see [`encode_datagrams`].
//! Wire version 1 (ALPN `lanlink/1`). Version 0 used a bare u16 service index per datagram.

use crate::Service;
use bytes::{BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;
use std::time::{Duration, Instant};

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

/// Size of [`DatagramHeader`] on the wire.
pub const DATAGRAM_HEADER_LEN: usize = 10;

/// Largest payload a UDP datagram can carry (IPv4, 65535 minus IP and UDP headers).
pub const MAX_UDP_PAYLOAD: usize = 65_507;

/// Partially reassembled packets are dropped after this long. Games resend or move on; a
/// packet this late is useless anyway.
const REASSEMBLY_TIMEOUT: Duration = Duration::from_secs(2);

/// Packets being reassembled at once, per connection direction.
const MAX_PARTIAL: usize = 64;

/// Header of every UDP datagram, all fields big endian:
/// `service: u16 | flow: u32 | msg: u16 | index: u8 | count: u8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatagramHeader {
    /// Index of the service in the host's advertised list.
    pub service: u16,
    /// Chosen by the client, one per local sender address. Replies carry the same id.
    pub flow: u32,
    /// Message id within the flow and direction, tells fragments of different packets apart.
    /// Only meaningful when `count > 1`.
    pub msg: u16,
    /// Fragment index, `< count`.
    pub index: u8,
    /// Number of fragments of this packet, 1 when unfragmented.
    pub count: u8,
}

impl DatagramHeader {
    fn put(&self, buf: &mut BytesMut) {
        buf.put_u16(self.service);
        buf.put_u32(self.flow);
        buf.put_u16(self.msg);
        buf.put_u8(self.index);
        buf.put_u8(self.count);
    }
}

/// Encode one UDP payload into datagrams of at most `max` bytes each (use
/// `Connection::max_datagram_size`, it follows the path MTU). Returns None when the payload
/// would need more than 255 fragments, i.e. `max` is unusually small.
///
/// Oversized payloads are fragmented over datagrams rather than sent on a QUIC stream.
/// Game traffic wants UDP semantics: low latency, and loss is fine but delay is not. A stream
/// is reliable and ordered, so one lost packet stalls everything behind it until it is
/// retransmitted, and the game then gets stale state late. Fragments keep the packet
/// unreliable: if one is lost the whole packet is dropped (as with IP fragmentation) and
/// nothing else waits for it. Small packets, the vast majority, still go out as a single
/// datagram.
pub fn encode_datagrams(
    service: u16,
    flow: u32,
    msg: u16,
    payload: &[u8],
    max: usize,
) -> Option<Vec<Bytes>> {
    let chunk = max.checked_sub(DATAGRAM_HEADER_LEN).filter(|&c| c > 0)?;
    let count = payload.len().div_ceil(chunk).max(1);
    let count = u8::try_from(count).ok()?;
    let header = DatagramHeader {
        service,
        flow,
        msg: if count == 1 { 0 } else { msg },
        index: 0,
        count,
    };
    let mut chunks = payload.chunks(chunk);
    let out = (0..count)
        .map(|index| {
            let part = chunks.next().unwrap_or_default();
            let mut buf = BytesMut::with_capacity(DATAGRAM_HEADER_LEN + part.len());
            DatagramHeader { index, ..header }.put(&mut buf);
            buf.extend_from_slice(part);
            buf.freeze()
        })
        .collect();
    Some(out)
}

/// Split a datagram into header and payload.
pub fn decode_datagram(data: &[u8]) -> Option<(DatagramHeader, &[u8])> {
    let (h, payload) = data.split_first_chunk::<DATAGRAM_HEADER_LEN>()?;
    let header = DatagramHeader {
        service: u16::from_be_bytes([h[0], h[1]]),
        flow: u32::from_be_bytes([h[2], h[3], h[4], h[5]]),
        msg: u16::from_be_bytes([h[6], h[7]]),
        index: h[8],
        count: h[9],
    };
    (header.index < header.count).then_some((header, payload))
}

struct Partial {
    parts: Vec<Option<Vec<u8>>>,
    missing: usize,
    size: usize,
    started: Instant,
}

/// Rebuilds fragmented UDP packets. One per connection direction.
#[derive(Default)]
pub struct Reassembler {
    partial: HashMap<(u16, u32, u16), Partial>,
}

impl Reassembler {
    /// Feed one decoded datagram. Returns the full packet once all its fragments arrived
    /// (immediately for unfragmented packets).
    pub fn accept<'a>(&mut self, h: &DatagramHeader, payload: &'a [u8]) -> Option<Cow<'a, [u8]>> {
        if h.count == 1 {
            return Some(Cow::Borrowed(payload));
        }
        let key = (h.service, h.flow, h.msg);
        let count = h.count as usize;
        let stale = self
            .partial
            .get(&key)
            .is_some_and(|p| p.parts.len() != count || p.started.elapsed() > REASSEMBLY_TIMEOUT);
        if stale {
            // The message id wrapped around, or a fragment of a long-dead packet.
            self.partial.remove(&key);
        }
        if !self.partial.contains_key(&key) {
            self.make_room();
            self.partial.insert(
                key,
                Partial {
                    parts: vec![None; count],
                    missing: count,
                    size: 0,
                    started: Instant::now(),
                },
            );
        }
        let p = self.partial.get_mut(&key)?;
        let slot = &mut p.parts[h.index as usize];
        if slot.is_some() {
            return None;
        }
        p.size += payload.len();
        if p.size > MAX_UDP_PAYLOAD {
            self.partial.remove(&key);
            return None;
        }
        *slot = Some(payload.to_vec());
        p.missing -= 1;
        if p.missing > 0 {
            return None;
        }
        let p = self.partial.remove(&key)?;
        let mut out = Vec::with_capacity(p.size);
        for part in p.parts.into_iter().flatten() {
            out.extend_from_slice(&part);
        }
        Some(Cow::Owned(out))
    }

    /// Drop timed out packets, then the oldest one if still full.
    fn make_room(&mut self) {
        if self.partial.len() < MAX_PARTIAL {
            return;
        }
        self.partial
            .retain(|_, p| p.started.elapsed() <= REASSEMBLY_TIMEOUT);
        if self.partial.len() >= MAX_PARTIAL {
            let oldest = self
                .partial
                .iter()
                .min_by_key(|(_, p)| p.started)
                .map(|(k, _)| *k);
            if let Some(k) = oldest {
                self.partial.remove(&k);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reassemble(r: &mut Reassembler, frags: &[Bytes]) -> Option<Vec<u8>> {
        let mut out = None;
        for f in frags {
            let (h, p) = decode_datagram(f).unwrap();
            if let Some(full) = r.accept(&h, p) {
                assert!(out.is_none(), "completed twice");
                out = Some(full.into_owned());
            }
        }
        out
    }

    #[test]
    fn small_payload_is_one_datagram() {
        let frags = encode_datagrams(3, 7, 9, b"hello", 1200).unwrap();
        assert_eq!(frags.len(), 1);
        let (h, p) = decode_datagram(&frags[0]).unwrap();
        assert_eq!((h.service, h.flow, h.count, p), (3, 7, 1, &b"hello"[..]));
    }

    #[test]
    fn fragments_round_trip_in_any_order() {
        let payload: Vec<u8> = (0..20_000u32).map(|i| i as u8).collect();
        let mut frags = encode_datagrams(1, 42, 5, &payload, 1200).unwrap();
        assert_eq!(
            frags.len(),
            payload.len().div_ceil(1200 - DATAGRAM_HEADER_LEN)
        );
        assert!(frags.iter().all(|f| f.len() <= 1200));
        frags.reverse();
        let mut r = Reassembler::default();
        assert_eq!(reassemble(&mut r, &frags).as_deref(), Some(&payload[..]));
        assert!(r.partial.is_empty());
    }

    #[test]
    fn lost_fragment_drops_only_that_packet() {
        let a = vec![1u8; 5000];
        let b = vec![2u8; 5000];
        let mut fa = encode_datagrams(0, 1, 1, &a, 1200).unwrap();
        let fb = encode_datagrams(0, 1, 2, &b, 1200).unwrap();
        fa.remove(2);
        let mut r = Reassembler::default();
        assert_eq!(reassemble(&mut r, &fa), None);
        assert_eq!(reassemble(&mut r, &fb).as_deref(), Some(&b[..]));
    }

    #[test]
    fn too_many_fragments_is_refused() {
        assert!(encode_datagrams(0, 0, 0, &[0u8; MAX_UDP_PAYLOAD], 100).is_none());
        assert!(encode_datagrams(0, 0, 0, b"x", DATAGRAM_HEADER_LEN).is_none());
    }
}
