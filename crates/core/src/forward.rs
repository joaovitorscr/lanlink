//! TCP <-> QUIC stream copying and UDP <-> datagram relaying.

use iroh::endpoint::{RecvStream, SendStream};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::{TcpStream, UdpSocket};

/// Client side UDP tunnel endpoint: local socket plus last local sender.
#[derive(Clone)]
pub struct UdpClient {
    pub socket: Arc<UdpSocket>,
    pub last_sender: Arc<Mutex<Option<SocketAddr>>>,
}

impl UdpClient {
    /// Deliver a payload received from the peer to the last local sender.
    pub async fn deliver(&self, payload: &[u8]) {
        let addr = *self.last_sender.lock().unwrap();
        if let Some(addr) = addr {
            if let Err(e) = self.socket.send_to(payload, addr).await {
                tracing::debug!("client udp send_to: {e}");
            }
        }
    }
}

/// Byte counters bumped per chunk by [`copy_counted`].
pub type Counters = Vec<Arc<std::sync::atomic::AtomicU64>>;

const TCP_CHUNK: usize = 16 * 1024;

/// Copy `r` into `w` chunk by chunk (no batching, so latency stays low), adding every chunk's
/// size to each counter. Shuts down `w` on EOF.
async fn copy_one_way<R, W>(mut r: R, mut w: W, counters: &Counters) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use std::sync::atomic::Ordering;
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
