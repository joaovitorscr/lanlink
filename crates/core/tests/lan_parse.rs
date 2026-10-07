use lanlink_core::lan::{format_announcement, parse_announcement};

#[test]
fn parses_minecraft_lan_announcements() {
    assert_eq!(
        parse_announcement("[MOTD]Alex - My World[/MOTD][AD]54321[/AD]"),
        Some(("Alex - My World".to_string(), 54321))
    );
    assert_eq!(parse_announcement("[MOTD]no port[/MOTD]"), None);
    assert_eq!(parse_announcement("[MOTD]x[/MOTD][AD]99999[/AD]"), None);
    assert_eq!(parse_announcement(""), None);
    let msg = format_announcement("Arthur - minecraft (lanlink)", 25565);
    assert_eq!(
        parse_announcement(&msg),
        Some(("Arthur - minecraft (lanlink)".to_string(), 25565))
    );
}
