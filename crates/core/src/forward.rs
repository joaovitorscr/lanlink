//! TCP <-> QUIC stream copying and UDP <-> datagram relaying.

use crate::protocol::{encode_datagram, read_msg, write_msg, StreamHeader};
use crate::{Protocol, Service};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use tokio::net::{TcpStream, UdpSocket};

const UDP_BUF: usize = 64 * 1024;

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

/// Host side: a UDP socket "connected" to the local service for one (peer, service) pair.
/// Spawns a task relaying replies back to the peer as datagrams.
pub async fn host_udp_socket(
    conn: Connection,
    index: u16,
    port: u16,
) -> anyhow::Result<Arc<UdpSocket>> {
    let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    sock.connect((Ipv4Addr::LOCALHOST, port)).await?;
    let sock = Arc::new(sock);
    let reader = sock.clone();
    tokio::spawn(async move {
        let mut buf = vec![0u8; UDP_BUF];
        loop {
            tokio::select! {
                _ = conn.closed() => break,
                res = reader.recv(&mut buf) => match res {
                    Ok(n) => {
                        if let Err(e) = conn.send_datagram(encode_datagram(index, &buf[..n]).into()) {
                            tracing::debug!("udp send_datagram: {e}");
                        }
                    }
                    Err(e) => {
                        tracing::debug!("host udp recv: {e}");
                        break;
                    }
                },
            }
        }
    });
    Ok(sock)
}

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
