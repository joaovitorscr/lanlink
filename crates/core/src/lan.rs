//! Minecraft "Open to LAN" discovery: multicast 224.0.2.60:4445, payload
//! `[MOTD]<text>[/MOTD][AD]<port>[/AD]`.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

pub const LAN_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 2, 60);
pub const LAN_PORT: u16 = 4445;
/// Suffix of every MOTD we announce, so our own detector can ignore it.
pub const OUR_MARKER: &str = "(lanlink)";

/// Parse a LAN announcement into (motd, port).
pub fn parse_announcement(msg: &str) -> Option<(String, u16)> {
    let motd = between(msg, "[MOTD]", "[/MOTD]")?;
    let port = between(msg, "[AD]", "[/AD]")?.trim().parse().ok()?;
    Some((motd.to_string(), port))
}

/// Build a LAN announcement.
pub fn format_announcement(motd: &str, port: u16) -> String {
    format!("[MOTD]{motd}[/MOTD][AD]{port}[/AD]")
}

fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let i = s.find(start)? + start.len();
    let j = s[i..].find(end)? + i;
    Some(&s[i..j])
}

/// Bind the shared Minecraft LAN listener socket (reuse addr/port so Minecraft and others can too).
pub fn bind_listener() -> std::io::Result<tokio::net::UdpSocket> {
    use socket2::{Domain, Protocol, SockAddr, Socket, Type};
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    sock.set_reuse_port(true)?;
    sock.bind(&SockAddr::from(SocketAddrV4::new(
        Ipv4Addr::UNSPECIFIED,
        LAN_PORT,
    )))?;
    sock.join_multicast_v4(&LAN_GROUP, &Ipv4Addr::UNSPECIFIED)?;
    // Also join on loopback so announcements sent over 127.0.0.1 are seen (best effort).
    let _ = sock.join_multicast_v4(&LAN_GROUP, &Ipv4Addr::LOCALHOST);
    sock.set_nonblocking(true)?;
    tokio::net::UdpSocket::from_std(sock.into())
}

/// Socket used to announce tunnels. Sends over loopback so the announcement's source address
/// is 127.0.0.1, which is where the tunnel listens.
pub fn bind_announcer() -> std::io::Result<tokio::net::UdpSocket> {
    use socket2::{Domain, Protocol, SockAddr, Socket, Type};
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_multicast_loop_v4(true)?;
    sock.set_multicast_ttl_v4(1)?;
    if sock.set_multicast_if_v4(&Ipv4Addr::LOCALHOST).is_ok() {
        sock.bind(&SockAddr::from(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)))?;
    } else {
        sock.bind(&SockAddr::from(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)))?;
    }
    sock.set_nonblocking(true)?;
    tokio::net::UdpSocket::from_std(sock.into())
}

pub fn group_addr() -> SocketAddr {
    SocketAddr::from((LAN_GROUP, LAN_PORT))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_announcement() {
        assert_eq!(
            parse_announcement("[MOTD]Steve - New World[/MOTD][AD]51234[/AD]"),
            Some(("Steve - New World".to_string(), 51234))
        );
        assert_eq!(
            parse_announcement("[MOTD][/MOTD][AD] 25565 [/AD]")
                .unwrap()
                .1,
            25565
        );
        assert_eq!(parse_announcement("[MOTD]x[/MOTD]"), None);
        assert_eq!(parse_announcement("[MOTD]x[/MOTD][AD]abc[/AD]"), None);
        assert_eq!(parse_announcement("[AD]1[/AD]"), None);
        assert_eq!(parse_announcement("garbage"), None);
        let s = format_announcement("a (lanlink)", 7);
        assert_eq!(parse_announcement(&s), Some(("a (lanlink)".to_string(), 7)));
    }
}
