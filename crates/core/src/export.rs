//! Config export and import files.
//!
//! An export is the whole [`Config`] in a small versioned envelope:
//!
//! ```json
//! { "lanlink_export": 1, "exported_at": "2026-10-08T12:00:00Z", "app_version": "0.0.2",
//!   "config": { "allowed_peers": [...], ... } }
//! ```
//!
//! The identity (`identity.key`) is a separate file and never part of an export, so an
//! imported config never changes who this node is.

use crate::{Config, NodeId};
use anyhow::{bail, Context};
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

/// Current envelope version. Files with a higher version are rejected.
pub const FORMAT_VERSION: u64 = 1;

/// How an imported config is combined with the current one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    /// The imported config becomes the config.
    Replace,
    /// Keep what is here and add what the file has; the file wins on conflicts.
    Merge,
}

#[derive(Serialize)]
struct Envelope<'a> {
    lanlink_export: u64,
    exported_at: String,
    app_version: &'a str,
    config: &'a Config,
}

/// Pretty JSON export of `config`, stamped with the current time and version.
pub fn export(config: &Config) -> String {
    let env = Envelope {
        lanlink_export: FORMAT_VERSION,
        exported_at: rfc3339(SystemTime::now()),
        app_version: crate::build_info::VERSION,
        config,
    };
    serde_json::to_string_pretty(&env).expect("config serializes")
}

/// Default file name for an export, e.g. `lanlink-config-2026-10-08.json`.
pub fn default_file_name() -> String {
    format!("lanlink-config-{}.json", &rfc3339(SystemTime::now())[..10])
}

/// A parsed export file.
#[derive(Debug, Clone)]
pub struct Import {
    pub exported_at: Option<String>,
    pub app_version: Option<String>,
    pub config: Config,
    /// The `config` object as written, to tell missing settings from defaults and to count
    /// fields this version does not know yet.
    raw: serde_json::Map<String, serde_json::Value>,
}

/// What an import file contains, for a confirmation prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub peers: usize,
    pub services: usize,
    pub tunnels: usize,
    /// Present when the file has a `networks` list.
    pub networks: Option<usize>,
}

impl Summary {
    /// e.g. "3 peers, 2 services, 1 saved tunnel".
    pub fn describe(&self) -> String {
        let mut parts = vec![
            plural(self.peers, "peer"),
            plural(self.services, "service"),
            plural(self.tunnels, "saved tunnel"),
        ];
        if let Some(n) = self.networks {
            parts.push(plural(n, "network"));
        }
        parts.join(", ")
    }
}

fn plural(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

/// Parse and check an export file.
pub fn parse(bytes: &[u8]) -> anyhow::Result<Import> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).context("not a lanlink export file: invalid JSON")?;
    let serde_json::Value::Object(mut obj) = value else {
        bail!("not a lanlink export file: expected a JSON object");
    };
    let Some(version) = obj.get("lanlink_export") else {
        bail!("not a lanlink export file: no \"lanlink_export\" field");
    };
    let version = version
        .as_u64()
        .filter(|v| *v >= 1)
        .with_context(|| format!("invalid export format version {version}"))?;
    if version > FORMAT_VERSION {
        bail!(
            "this file was exported by a newer lanlink (format {version}, this version reads \
             up to {FORMAT_VERSION}); update lanlink to import it"
        );
    }
    let text = |obj: &serde_json::Map<String, serde_json::Value>, key: &str| {
        obj.get(key).and_then(|v| v.as_str()).map(str::to_string)
    };
    let exported_at = text(&obj, "exported_at");
    let app_version = text(&obj, "app_version");
    let raw = match obj.remove("config") {
        Some(serde_json::Value::Object(raw)) => raw,
        Some(_) => bail!("invalid export file: \"config\" is not an object"),
        None => bail!("invalid export file: no \"config\" field"),
    };
    let config: Config = serde_json::from_value(serde_json::Value::Object(raw.clone()))
        .context("invalid export file: bad \"config\"")?;
    validate(&config)?;
    Ok(Import {
        exported_at,
        app_version,
        config,
        raw,
    })
}

/// Reject values the node would refuse at runtime, naming the bad entry.
pub(crate) fn validate(c: &Config) -> anyhow::Result<()> {
    for p in &c.allowed_peers {
        p.parse::<NodeId>()
            .with_context(|| format!("invalid peer id {p:?}"))?;
    }
    for s in &c.services {
        anyhow::ensure!(!s.name.trim().is_empty(), "a service has an empty name");
        anyhow::ensure!(s.port != 0, "service {:?} has port 0", s.name);
    }
    for t in &c.saved_tunnels {
        t.peer
            .parse::<NodeId>()
            .with_context(|| format!("saved tunnel {:?} has an invalid peer id", t.service))?;
    }
    if let Some(u) = &c.relay_url {
        u.parse::<iroh::RelayUrl>()
            .with_context(|| format!("invalid relay url {u}"))?;
    }
    Ok(())
}

impl Import {
    pub fn summary(&self) -> Summary {
        Summary {
            peers: self.config.allowed_peers.len(),
            services: self.config.services.len(),
            tunnels: self.config.saved_tunnels.len(),
            networks: self
                .raw
                .get("networks")
                .and_then(|n| n.as_array())
                .map(Vec::len),
        }
    }

    /// The config that results from importing this file over `current`.
    pub fn resolve(&self, current: &Config, mode: ImportMode) -> Config {
        match mode {
            ImportMode::Replace => self.config.clone(),
            ImportMode::Merge => self.merge(current),
        }
    }

    /// Union of both configs, the file winning on conflicts. Lists are deduplicated by
    /// their natural keys (peer id, service name, peer + service); settings are taken from
    /// the file only when it sets them.
    fn merge(&self, current: &Config) -> Config {
        // Destructured so a new Config field fails to compile here until it has a merge rule.
        let Config {
            allowed_peers,
            peer_names,
            services,
            relay_url,
            display_name,
            saved_tunnels,
            disable_lan_detection,
            latency_log,
        } = self.config.clone();
        let mut out = current.clone();
        for p in allowed_peers {
            if !out.allowed_peers.contains(&p) {
                out.allowed_peers.push(p);
            }
        }
        out.peer_names.extend(peer_names);
        for s in services {
            match out.services.iter_mut().find(|x| x.name == s.name) {
                Some(x) => *x = s,
                None => out.services.push(s),
            }
        }
        for t in saved_tunnels {
            match out
                .saved_tunnels
                .iter_mut()
                .find(|x| x.peer == t.peer && x.service == t.service)
            {
                Some(x) => *x = t,
                None => out.saved_tunnels.push(t),
            }
        }
        if relay_url.is_some() {
            out.relay_url = relay_url;
        }
        if display_name.is_some() {
            out.display_name = display_name;
        }
        if self.raw.contains_key("disable_lan_detection") {
            out.disable_lan_detection = disable_lan_detection;
        }
        if self.raw.contains_key("latency_log") {
            out.latency_log = latency_log;
        }
        out
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` (UTC).
fn rfc3339(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let rem = secs % 86_400;
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        crate::stats::civil_date(secs / 86_400),
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Protocol, SavedTunnel, SecretKey, Service};

    fn id() -> String {
        SecretKey::generate().public().to_string()
    }

    fn tunnel(peer: &str, service: &str, port: u16) -> SavedTunnel {
        SavedTunnel {
            peer: peer.into(),
            service: service.into(),
            protocol: Protocol::Tcp,
            local_port: port,
            auto_open: true,
        }
    }

    fn envelope(config: &str) -> String {
        format!(r#"{{"lanlink_export":1,"exported_at":"x","app_version":"0","config":{config}}}"#)
    }

    #[test]
    fn rfc3339_format() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        let t = UNIX_EPOCH + std::time::Duration::from_secs(1_791_460_861);
        assert_eq!(rfc3339(t), "2026-10-08T12:01:01Z");
    }

    #[test]
    fn export_round_trips_without_secrets() {
        let (a, b) = (id(), id());
        let mut c = Config {
            allowed_peers: vec![a.clone(), b.clone()],
            services: vec![Service::new("mc", Protocol::Tcp, 25565)],
            saved_tunnels: vec![tunnel(&a, "web", 8080)],
            display_name: Some("me".into()),
            latency_log: true,
            ..Default::default()
        };
        c.peer_names.insert(a.clone(), "Alice".into());
        let text = export(&c);

        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            ["app_version", "config", "exported_at", "lanlink_export"]
        );
        assert!(!text.contains("secret") && !text.contains("identity"));

        let imp = parse(text.as_bytes()).unwrap();
        assert_eq!(imp.app_version.as_deref(), Some(crate::build_info::VERSION));
        assert_eq!(
            serde_json::to_value(&imp.config).unwrap(),
            serde_json::to_value(&c).unwrap()
        );
        assert_eq!(
            imp.summary(),
            Summary {
                peers: 2,
                services: 1,
                tunnels: 1,
                networks: None
            }
        );
        assert_eq!(
            imp.summary().describe(),
            "2 peers, 1 service, 1 saved tunnel"
        );
    }

    #[test]
    fn replace_takes_the_file() {
        let (a, b) = (id(), id());
        let current = Config {
            allowed_peers: vec![a],
            services: vec![Service::new("old", Protocol::Tcp, 1)],
            display_name: Some("here".into()),
            latency_log: true,
            ..Default::default()
        };
        let imp = parse(envelope(&format!(r#"{{"allowed_peers":["{b}"]}}"#)).as_bytes()).unwrap();
        let out = imp.resolve(&current, ImportMode::Replace);
        assert_eq!(out.allowed_peers, vec![b]);
        assert!(out.services.is_empty());
        assert_eq!(out.display_name, None);
        assert!(!out.latency_log);
    }

    #[test]
    fn merge_unions_and_file_wins() {
        let (a, b, c) = (id(), id(), id());
        let mut current = Config {
            allowed_peers: vec![a.clone(), b.clone()],
            services: vec![
                Service::new("mc", Protocol::Tcp, 25565),
                Service::new("keep", Protocol::Udp, 9000),
            ],
            saved_tunnels: vec![tunnel(&a, "web", 8080), tunnel(&b, "web", 8081)],
            display_name: Some("here".into()),
            relay_url: Some("https://relay.example.com".into()),
            disable_lan_detection: true,
            latency_log: true,
            ..Default::default()
        };
        current.peer_names.insert(a.clone(), "Alice".into());
        current.peer_names.insert(b.clone(), "Bob".into());

        let mut file = Config {
            allowed_peers: vec![b.clone(), c.clone()],
            services: vec![
                Service::new("mc", Protocol::Tcp, 25566),
                Service::new("new", Protocol::Tcp, 7000),
            ],
            saved_tunnels: vec![tunnel(&a, "web", 9090), tunnel(&c, "db", 5432)],
            ..Default::default()
        };
        file.peer_names.insert(b.clone(), "Robert".into());
        file.peer_names.insert(c.clone(), "Carol".into());
        let mut raw = serde_json::to_value(&file).unwrap();
        // A hand-written file that leaves the LAN setting out keeps ours.
        raw.as_object_mut().unwrap().remove("disable_lan_detection");
        let imp = parse(envelope(&raw.to_string()).as_bytes()).unwrap();
        let out = imp.resolve(&current, ImportMode::Merge);

        assert_eq!(out.allowed_peers, vec![a.clone(), b.clone(), c.clone()]);
        assert_eq!(out.peer_names[&a], "Alice");
        assert_eq!(out.peer_names[&b], "Robert");
        assert_eq!(out.peer_names[&c], "Carol");
        let svcs: Vec<(&str, u16)> = out
            .services
            .iter()
            .map(|s| (s.name.as_str(), s.port))
            .collect();
        assert_eq!(svcs, [("mc", 25566), ("keep", 9000), ("new", 7000)]);
        let tuns: Vec<(&str, &str, u16)> = out
            .saved_tunnels
            .iter()
            .map(|t| (t.peer.as_str(), t.service.as_str(), t.local_port))
            .collect();
        assert_eq!(
            tuns,
            [
                (a.as_str(), "web", 9090),
                (b.as_str(), "web", 8081),
                (c.as_str(), "db", 5432)
            ]
        );
        // Unset in the file: ours stay.
        assert_eq!(out.display_name.as_deref(), Some("here"));
        assert_eq!(out.relay_url.as_deref(), Some("https://relay.example.com"));
        assert!(out.disable_lan_detection);
        // Set in the file (false is written explicitly): the file wins.
        assert!(!out.latency_log);
    }

    #[test]
    fn merge_takes_set_scalars() {
        let current = Config {
            display_name: Some("here".into()),
            ..Default::default()
        };
        let imp = parse(
            envelope(r#"{"allowed_peers":[],"display_name":"there","relay_url":"https://r.example.com"}"#)
                .as_bytes(),
        )
        .unwrap();
        let out = imp.resolve(&current, ImportMode::Merge);
        assert_eq!(out.display_name.as_deref(), Some("there"));
        assert_eq!(out.relay_url.as_deref(), Some("https://r.example.com"));
    }

    #[test]
    fn counts_networks_when_present() {
        let imp = parse(envelope(r#"{"allowed_peers":[],"networks":[{},{}]}"#).as_bytes()).unwrap();
        assert_eq!(imp.summary().networks, Some(2));
        assert_eq!(
            imp.summary().describe(),
            "0 peers, 0 services, 0 saved tunnels, 2 networks"
        );
    }

    fn err(text: &str) -> String {
        format!("{:#}", parse(text.as_bytes()).unwrap_err())
    }

    #[test]
    fn rejects_bad_files() {
        assert!(
            err("{not json").contains("invalid JSON"),
            "{}",
            err("{not json")
        );
        assert!(err("[1,2]").contains("expected a JSON object"));
        assert!(err(r#"{"allowed_peers":[]}"#).contains("no \"lanlink_export\""));
        assert!(err(r#"{"lanlink_export":"one","config":{}}"#).contains("invalid export format"));
        assert!(err(r#"{"lanlink_export":0,"config":{}}"#).contains("invalid export format"));
        let newer = err(r#"{"lanlink_export":2,"config":{"allowed_peers":[]}}"#);
        assert!(newer.contains("newer lanlink"), "{newer}");
        assert!(err(r#"{"lanlink_export":1}"#).contains("no \"config\""));
        assert!(err(r#"{"lanlink_export":1,"config":[]}"#).contains("not an object"));
        // Config without its required allowlist.
        let missing = err(&envelope(r#"{"services":[]}"#));
        assert!(missing.contains("allowed_peers"), "{missing}");
        // Service missing its port.
        let missing = err(&envelope(
            r#"{"allowed_peers":[],"services":[{"name":"mc","protocol":"tcp"}]}"#,
        ));
        assert!(missing.contains("port"), "{missing}");
        assert!(err(&envelope(r#"{"allowed_peers":["nope"]}"#)).contains("invalid peer id"));
        assert!(err(&envelope(
            r#"{"allowed_peers":[],"services":[{"name":"mc","protocol":"tcp","port":0}]}"#
        ))
        .contains("port 0"));
        assert!(err(&envelope(r#"{"allowed_peers":[],"relay_url":"::"}"#))
            .contains("invalid relay url"));
    }

    #[test]
    fn metadata_is_optional() {
        let imp = parse(br#"{"lanlink_export":1,"config":{"allowed_peers":[]}}"#).unwrap();
        assert_eq!(imp.exported_at, None);
        assert!(default_file_name().starts_with("lanlink-config-"));
    }
}
