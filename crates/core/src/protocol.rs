//! Wire protocol over a lanlink QUIC connection.
//!
//! Control: the FIRST bidirectional stream opened by the dialer is the control stream.
//! Messages are length-prefixed (u32 BE) JSON `ControlMsg`, tagged by a `"type"` field,
//! e.g. `{"type":"Ping","t":123}`. Unknown types and unknown fields are ignored, so a newer
//! build can add messages and fields without breaking older ones.
//! Data: every later bi stream begins with a `StreamHeader` (length-prefixed JSON), then raw bytes.
//! UDP: QUIC datagrams, first byte(s) = service index (u16 BE) then payload.

use crate::Service;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ControlMsg {
    Hello {
        name: Option<String>,
    },
    Services {
        services: Vec<Service>,
    },
    /// `t` is the sender's clock in microseconds; it is echoed back in `Pong`.
    Ping {
        t: u64,
    },
    Pong {
        t: u64,
    },
    /// A message type this build does not know (sent by a newer build). Ignored.
    #[serde(other)]
    Unknown,
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

/// Read the body of one u32 BE length-prefixed frame.
pub async fn read_frame<R>(r: &mut R) -> anyhow::Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    anyhow::ensure!(len <= MAX_FRAME, "frame too large: {len}");
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    Ok(body)
}

/// Read a u32 BE length-prefixed JSON message.
pub async fn read_msg<R, T>(r: &mut R) -> anyhow::Result<T>
where
    R: tokio::io::AsyncRead + Unpin,
    T: serde::de::DeserializeOwned,
{
    Ok(serde_json::from_slice(&read_frame(r).await?)?)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Protocol;

    #[test]
    fn known_messages_round_trip() {
        let msgs = [
            ControlMsg::Hello {
                name: Some("ana".into()),
            },
            ControlMsg::Services {
                services: vec![Service::new("minecraft", Protocol::Tcp, 25565)],
            },
            ControlMsg::Ping { t: 7 },
            ControlMsg::Pong { t: 7 },
        ];
        for msg in msgs {
            let json = serde_json::to_string(&msg).unwrap();
            let back: ControlMsg = serde_json::from_str(&json).unwrap();
            assert_eq!(back, msg, "{json}");
        }
        assert_eq!(
            serde_json::to_string(&ControlMsg::Ping { t: 7 }).unwrap(),
            r#"{"type":"Ping","t":7}"#
        );
    }

    #[test]
    fn unknown_message_is_unknown() {
        for json in [
            r#"{"type":"Bye"}"#,
            r#"{"type":"Chat","text":"hi","to":["a","b"],"meta":{"x":1}}"#,
        ] {
            let msg: ControlMsg = serde_json::from_str(json).unwrap();
            assert_eq!(msg, ControlMsg::Unknown, "{json}");
        }
        // And it can be written back without error.
        let json = serde_json::to_string(&ControlMsg::Unknown).unwrap();
        let back: ControlMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ControlMsg::Unknown);
    }

    #[test]
    fn extra_fields_are_ignored() {
        let msg: ControlMsg =
            serde_json::from_str(r#"{"type":"Ping","t":5,"sent_by":"newer build"}"#).unwrap();
        assert_eq!(msg, ControlMsg::Ping { t: 5 });

        let msg: ControlMsg = serde_json::from_str(
            r#"{"type":"Services","services":[{"name":"mc","protocol":"tcp","port":25565,"icon":"pick"}],"v":2}"#,
        )
        .unwrap();
        let ControlMsg::Services { services } = msg else {
            panic!("expected Services, got {msg:?}");
        };
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].port, 25565);
        assert!(services[0].enabled);
        assert_eq!(services[0].host, None);

        let msg: ControlMsg = serde_json::from_str(r#"{"type":"Hello","avatar":"x"}"#).unwrap();
        assert_eq!(msg, ControlMsg::Hello { name: None });
    }

    #[tokio::test]
    async fn framed_unknown_message_reads_as_unknown() {
        let mut buf = Vec::new();
        write_msg(&mut buf, &serde_json::json!({"type": "Future", "n": 1}))
            .await
            .unwrap();
        write_msg(&mut buf, &ControlMsg::Ping { t: 1 })
            .await
            .unwrap();
        let mut r = buf.as_slice();
        let a: ControlMsg = read_msg(&mut r).await.unwrap();
        let b: ControlMsg = read_msg(&mut r).await.unwrap();
        assert_eq!(a, ControlMsg::Unknown);
        assert_eq!(b, ControlMsg::Ping { t: 1 });
    }
}
