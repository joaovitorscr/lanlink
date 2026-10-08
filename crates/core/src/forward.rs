//! TCP <-> QUIC stream copying and UDP <-> datagram relaying.

use crate::protocol::{encode_datagrams, read_msg, write_msg, StreamHeader};
use crate::{Protocol, Service};
use anyhow::Context;
use iroh::endpoint::{Connection, RecvStream, SendDatagramError, SendStream};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::net::{TcpStream, UdpSocket};

/// Receive buffer for local UDP sockets, large enough for any UDP payload.
pub(crate) const UDP_BUF: usize = 64 * 1024;

/// Client side: forward one accepted local TCP socket over a fresh bi stream.
pub async fn client_tcp(
    conn: Connection,
    service: String,
    mut tcp: TcpStream,
) -> anyhow::Result<()> {
    tcp.set_nodelay(true)?;
    let (mut send, recv) = conn.open_bi().await?;
    write_msg(&mut send, &StreamHeader { service }).await?;
    let mut quic = tokio::io::join(recv, send);
    tokio::io::copy_bidirectional(&mut tcp, &mut quic).await?;
    Ok(())
}

/// Host side: handle an incoming data stream. Reads the header and connects to the local service.
pub async fn host_tcp(
    services: Vec<Service>,
    mut send: SendStream,
    mut recv: RecvStream,
) -> anyhow::Result<()> {
    let header: StreamHeader = read_msg(&mut recv).await?;
    let Some(svc) = services
        .iter()
        .find(|s| s.name == header.service && s.protocol == Protocol::Tcp)
    else {
        tracing::warn!(service = %header.service, "refusing stream for unknown service");
        let _ = send.reset(1u32.into());
        return Ok(());
    };
    let mut tcp = TcpStream::connect((Ipv4Addr::LOCALHOST, svc.port)).await?;
    tcp.set_nodelay(true)?;
    let mut quic = tokio::io::join(recv, send);
    tokio::io::copy_bidirectional(&mut tcp, &mut quic).await?;
    Ok(())
}

/// Send state of one UDP flow in one direction.
#[derive(Default)]
pub struct FlowSender {
    next_msg: AtomicU16,
    warned: AtomicBool,
}

impl FlowSender {
    /// Send one UDP payload as datagrams, fragmenting it above the connection's current
    /// datagram limit. The first payload a flow cannot send is logged as a warning, later
    /// ones at debug, so a game spamming large packets does not flood the log.
    pub fn send(&self, conn: &Connection, service: u16, flow: u32, payload: &[u8]) {
        let Err(e) = self.try_send(conn, service, flow, payload) else {
            return;
        };
        if conn.close_reason().is_some() || self.warned.swap(true, Ordering::Relaxed) {
            tracing::debug!(
                service,
                flow,
                len = payload.len(),
                "udp packet dropped: {e:#}"
            );
        } else {
            tracing::warn!(
                service,
                flow,
                len = payload.len(),
                "udp packet dropped, further drops on this flow are logged at debug: {e:#}"
            );
        }
    }

    fn try_send(
        &self,
        conn: &Connection,
        service: u16,
        flow: u32,
        payload: &[u8],
    ) -> anyhow::Result<()> {
        // The limit follows the path MTU and can shrink between reading it and sending.
        for _ in 0..2 {
            let max = conn
                .max_datagram_size()
                .context("peer does not accept datagrams")?;
            let msg = self.next_msg.fetch_add(1, Ordering::Relaxed);
            let frags = encode_datagrams(service, flow, msg, payload, max).with_context(|| {
                format!(
                    "{} bytes do not fit in 255 datagrams of {max} bytes",
                    payload.len()
                )
            })?;
            match conn.send_many_datagrams(&frags) {
                Ok(_) => return Ok(()),
                Err(SendDatagramError::TooLarge) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        anyhow::bail!("datagram too large for the current path")
    }
}

/// Client side: one local program (source address) talking through a UDP tunnel.
/// Replies carrying its flow id go back to `local`.
pub struct ClientFlow {
    pub id: u32,
    pub local: SocketAddr,
    pub socket: Arc<UdpSocket>,
    pub sender: FlowSender,
    /// Milliseconds since node start of the last packet in either direction.
    pub last_ms: AtomicU64,
}

impl ClientFlow {
    /// Deliver a payload received from the peer to this flow's local sender.
    pub async fn deliver(&self, payload: &[u8]) {
        if let Err(e) = self.socket.send_to(payload, self.local).await {
            tracing::debug!("client udp send_to: {e}");
        }
    }
}

/// Byte counters bumped per chunk by [`copy_counted`].
pub type Counters = Vec<Arc<AtomicU64>>;

const TCP_CHUNK: usize = 16 * 1024;

/// Copy `r` into `w` chunk by chunk (no batching, so latency stays low), adding every chunk's
/// size to each counter. Shuts down `w` on EOF.
async fn copy_one_way<R, W>(mut r: R, mut w: W, counters: &Counters) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0u8; TCP_CHUNK];
    loop {
        let n = r.read(&mut buf).await?;
        if n == 0 {
            let _ = w.shutdown().await;
            return Ok(());
        }
        w.write_all(&buf[..n]).await?;
        for c in counters {
            c.fetch_add(n as u64, Ordering::Relaxed);
        }
    }
}

/// Bidirectional TCP <-> QUIC copy with live byte counters.
/// `out` counts bytes tcp -> quic, `inn` counts bytes quic -> tcp.
pub async fn copy_counted(
    tcp: TcpStream,
    send: SendStream,
    recv: RecvStream,
    out: &Counters,
    inn: &Counters,
) -> anyhow::Result<()> {
    tcp.set_nodelay(true)?;
    let (tr, tw) = tcp.into_split();
    tokio::try_join!(copy_one_way(tr, send, out), copy_one_way(recv, tw, inn))?;
    Ok(())
}
