//! Networks: groups of friends who can reach each other's shared services.
//!
//! Fully peer to peer. The owner's node is the source of truth for a network's membership;
//! members keep a copy in their config so the UI works while the owner is offline, and
//! replace it whenever the owner pushes a new one (`ControlMsg::NetworkState`). Being a
//! member of a network we are also in makes a peer allowed, like `Config::allowed_peers`.
//!
//! Joining uses an invite code (see [`InviteCode`]): it carries the owner's NodeId, the
//! network id and a secret token, so the joiner can dial the owner and prove it was invited.

use crate::NodeId;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `n` random bytes as lowercase hex.
pub(crate) fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).expect("no system randomness");
    hex(&buf)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; N];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Color of a network's badge in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkColor {
    #[default]
    Blue,
    Green,
    Orange,
    Purple,
    Pink,
    Red,
    Teal,
    /// A color this build does not know (written by a newer build).
    #[serde(other)]
    Gray,
}

impl NetworkColor {
    pub const ALL: [NetworkColor; 8] = [
        NetworkColor::Blue,
        NetworkColor::Green,
        NetworkColor::Orange,
        NetworkColor::Purple,
        NetworkColor::Pink,
        NetworkColor::Red,
        NetworkColor::Teal,
        NetworkColor::Gray,
    ];
}

/// When an invite code stops working.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum InviteExpiry {
    #[default]
    Never,
    /// `hours` after the code was created.
    AfterHours { hours: u32 },
    /// After one join request used it.
    FirstUse,
}

/// What happens when someone uses an invite code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approval {
    /// The owner sees a request and allows or ignores it.
    #[default]
    AskMe,
    /// Joined at once.
    Auto,
}

/// An invite code the owner created. Only stored on the owner's node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    /// Random secret, 20 hex chars (80 bits).
    pub token: String,
    pub created_at: u64,
    #[serde(default)]
    pub expires: InviteExpiry,
    #[serde(default)]
    pub approval: Approval,
    #[serde(default)]
    pub revoked: bool,
    /// Join requests that used this code.
    #[serde(default)]
    pub uses: u32,
}

impl Invite {
    pub fn new(expires: InviteExpiry, approval: Approval) -> Self {
        Self {
            token: random_hex(TOKEN_LEN),
            created_at: now_secs(),
            expires,
            approval,
            revoked: false,
            uses: 0,
        }
    }

    /// Not revoked, not expired, not used up.
    pub fn is_valid(&self, now: u64) -> bool {
        !self.revoked
            && match self.expires {
                InviteExpiry::Never => true,
                InviteExpiry::AfterHours { hours } => {
                    now < self.created_at.saturating_add(u64::from(hours) * 3600)
                }
                InviteExpiry::FirstUse => self.uses == 0,
            }
    }
}

/// Network-wide rules, set by the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPolicy {
    /// Members (not only the owner) may share services with the network.
    #[serde(default = "yes")]
    pub members_share: bool,
    /// Members may see the invite code and pass it on.
    #[serde(default)]
    pub members_invite: bool,
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self {
            members_share: true,
            members_invite: false,
        }
    }
}

fn yes() -> bool {
    true
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    /// NodeId string.
    pub id: String,
    /// Name the member sent when joining (or the owner's display name). Display only.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub joined_at: u64,
}

/// A network, as stored in `Config::networks` and sent in `ControlMsg::NetworkState`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Network {
    /// Random 128 bit id, 32 hex chars.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub color: NetworkColor,
    /// NodeId string of the owner, the only node that can change the network.
    pub owner: String,
    /// Everyone in the network, the owner included.
    #[serde(default)]
    pub members: Vec<Member>,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub policy: NetworkPolicy,
    /// Owner only: invite codes. Never sent to members.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invites: Vec<Invite>,
    /// Owner only: people who used an "ask me" code and wait for approval. `joined_at` is
    /// when they asked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub join_requests: Vec<Member>,
    /// Members only: the owner's current invite code, when `policy.members_invite` is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_invite: Option<String>,
    /// Local: we asked to join and the owner has not approved us yet. Only the owner is
    /// known; it may connect to us so it can tell us when we are in.
    #[serde(default, skip_serializing_if = "is_false")]
    pub pending: bool,
    /// Local: we left, but the owner has not confirmed it yet (it was offline).
    #[serde(default, skip_serializing_if = "is_false")]
    pub leaving: bool,
}

/// Random part of an invite, in bytes.
const TOKEN_LEN: usize = 10;
const NETWORK_ID_LEN: usize = 16;

impl Network {
    pub fn new(name: impl Into<String>, owner: NodeId, owner_name: impl Into<String>) -> Self {
        let now = now_secs();
        Self {
            id: random_hex(NETWORK_ID_LEN),
            name: name.into(),
            color: NetworkColor::default(),
            owner: owner.to_string(),
            members: vec![Member {
                id: owner.to_string(),
                name: owner_name.into(),
                joined_at: now,
            }],
            created_at: now,
            policy: NetworkPolicy::default(),
            invites: Vec::new(),
            join_requests: Vec::new(),
            shared_invite: None,
            pending: false,
            leaving: false,
        }
    }

    pub fn owner_id(&self) -> Option<NodeId> {
        self.owner.parse().ok()
    }

    pub fn is_owner(&self, id: &NodeId) -> bool {
        self.owner == id.to_string()
    }

    pub fn has_member(&self, id: &NodeId) -> bool {
        let s = id.to_string();
        self.members.iter().any(|m| m.id == s)
    }

    pub fn member_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.members.iter().filter_map(|m| m.id.parse().ok())
    }

    /// Still in the network from our side: not waiting for approval, not leaving.
    pub fn active(&self) -> bool {
        !self.pending && !self.leaving
    }

    /// The newest invite that still works.
    pub fn current_invite(&self) -> Option<&Invite> {
        let now = now_secs();
        self.invites.iter().rev().find(|i| i.is_valid(now))
    }

    /// Code for the newest working invite (owner), or the one the owner shared (members).
    pub fn invite_code(&self) -> Option<String> {
        match self.current_invite() {
            Some(inv) => self.code_for(&inv.token),
            None => self.shared_invite.clone(),
        }
    }

    /// The invite code for `token` of this network.
    pub fn code_for(&self, token: &str) -> Option<String> {
        Some(
            InviteCode {
                owner: self.owner_id()?,
                network: self.id.clone(),
                token: token.to_string(),
            }
            .to_string(),
        )
    }

    /// The copy sent to members: no invites, no local flags; the current code only if
    /// members may invite.
    pub fn for_members(&self) -> Network {
        let shared_invite = if self.policy.members_invite {
            self.invite_code()
        } else {
            None
        };
        Network {
            invites: Vec::new(),
            join_requests: Vec::new(),
            shared_invite,
            pending: false,
            leaving: false,
            ..self.clone()
        }
    }
}

/// What a joiner pastes: `lanlink-` followed by base32 (RFC 4648, no padding) of
/// `version (1) | owner NodeId (32) | network id (16) | token (10) | checksum (2)`,
/// grouped in fours with dashes. Parsing ignores case, dashes and whitespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteCode {
    pub owner: NodeId,
    /// Network id, 32 hex chars.
    pub network: String,
    /// Invite token, 20 hex chars.
    pub token: String,
}

const CODE_PREFIX: &str = "lanlink-";
const CODE_VERSION: u8 = 1;
const B32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

fn checksum(data: &[u8]) -> [u8; 2] {
    // FNV-1a folded to 16 bits: catches typos, not an integrity guarantee.
    let mut h: u32 = 0x811c_9dc5;
    for b in data {
        h ^= u32::from(*b);
        h = h.wrapping_mul(0x0100_0193);
    }
    let h = (h >> 16) ^ (h & 0xffff);
    (h as u16).to_be_bytes()
}

fn base32(data: &[u8]) -> String {
    let mut out = String::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in data {
        acc = (acc << 8) | u32::from(*b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(B32[((acc >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(B32[((acc << (5 - bits)) & 31) as usize] as char);
    }
    out
}

fn unbase32(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.chars() {
        let c = c.to_ascii_uppercase();
        // Letters people mistype for digits.
        let c = match c {
            '0' => 'O',
            '1' => 'I',
            '8' => 'B',
            c => c,
        };
        let v = B32.iter().position(|&x| x as char == c)? as u32;
        acc = (acc << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
        acc &= (1 << bits) - 1;
    }
    Some(out)
}

impl InviteCode {
    /// Parse a pasted code. Tolerates case, dashes, spaces, and a missing prefix.
    pub fn parse(s: &str) -> anyhow::Result<InviteCode> {
        let bad = || anyhow::anyhow!("That doesn't look like a lanlink invite code.");
        let s = s.trim();
        // "lanlink" then a separator; the body always starts with 'A' (version 1).
        let word = CODE_PREFIX.trim_end_matches('-');
        let body = match s.get(..word.len()) {
            Some(p) if p.eq_ignore_ascii_case(word) => &s[word.len()..],
            _ => s,
        };
        let body: String = body
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .collect();
        let bytes = unbase32(&body).ok_or_else(bad)?;
        const LEN: usize = 1 + 32 + NETWORK_ID_LEN + TOKEN_LEN + 2;
        if bytes.len() != LEN {
            return Err(bad());
        }
        let (data, sum) = bytes.split_at(LEN - 2);
        if checksum(data) != sum {
            anyhow::bail!("This invite code has a typo. Ask your friend to copy it again.");
        }
        if data[0] != CODE_VERSION {
            anyhow::bail!("This invite code is from a newer lanlink. Update lanlink first.");
        }
        let owner: [u8; 32] = data[1..33].try_into()?;
        let owner = NodeId::from_bytes(&owner).map_err(|_| bad())?;
        Ok(InviteCode {
            owner,
            network: hex(&data[33..33 + NETWORK_ID_LEN]),
            token: hex(&data[33 + NETWORK_ID_LEN..]),
        })
    }
}

impl std::fmt::Display for InviteCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let network = unhex::<NETWORK_ID_LEN>(&self.network).ok_or(std::fmt::Error)?;
        let token = unhex::<TOKEN_LEN>(&self.token).ok_or(std::fmt::Error)?;
        let mut data = vec![CODE_VERSION];
        data.extend_from_slice(self.owner.as_bytes());
        data.extend_from_slice(&network);
        data.extend_from_slice(&token);
        let sum = checksum(&data);
        data.extend_from_slice(&sum);
        let body = base32(&data);
        let groups: Vec<&str> = body
            .as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).unwrap_or_default())
            .collect();
        write!(f, "{CODE_PREFIX}{}", groups.join("-"))
    }
}

impl std::str::FromStr for InviteCode {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Self> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SecretKey;

    fn net() -> Network {
        Network::new("Crew", SecretKey::generate().public(), "Ana")
    }

    #[test]
    fn invite_code_round_trips() {
        let mut n = net();
        n.invites
            .push(Invite::new(InviteExpiry::Never, Approval::Auto));
        let code = n.invite_code().unwrap();
        assert!(code.starts_with("lanlink-"), "{code}");
        let parsed = InviteCode::parse(&code).unwrap();
        assert_eq!(parsed.owner, n.owner_id().unwrap());
        assert_eq!(parsed.network, n.id);
        assert_eq!(parsed.token, n.invites[0].token);
        assert_eq!(parsed.to_string(), code);
    }

    #[test]
    fn invite_code_is_forgiving() {
        let mut n = net();
        n.invites
            .push(Invite::new(InviteExpiry::Never, Approval::Auto));
        let code = n.invite_code().unwrap();
        let want = InviteCode::parse(&code).unwrap();
        let lower = code.to_lowercase();
        let no_dashes = code.replace('-', "");
        let spaced = format!("  {}\n", code.replace('-', " "));
        let no_prefix = code.trim_start_matches("lanlink-").to_string();
        for s in [lower, no_dashes, spaced, no_prefix] {
            assert_eq!(InviteCode::parse(&s).unwrap(), want, "{s}");
        }
    }

    #[test]
    fn invite_code_rejects_garbage_and_typos() {
        let mut n = net();
        n.invites
            .push(Invite::new(InviteExpiry::Never, Approval::Auto));
        let code = n.invite_code().unwrap();
        assert!(InviteCode::parse("").is_err());
        assert!(InviteCode::parse("MCRW-7K2P-Q9ZD").is_err());
        assert!(InviteCode::parse(&code[..code.len() - 4]).is_err());
        // Change one character of the body.
        let mut chars: Vec<char> = code.chars().collect();
        let i = 20;
        chars[i] = if chars[i] == 'A' { 'B' } else { 'A' };
        let typo: String = chars.into_iter().collect();
        assert!(InviteCode::parse(&typo).is_err());
    }

    #[test]
    fn invite_validity() {
        let mut inv = Invite::new(InviteExpiry::AfterHours { hours: 1 }, Approval::AskMe);
        let t = inv.created_at;
        assert!(inv.is_valid(t + 3599));
        assert!(!inv.is_valid(t + 3600));
        inv.expires = InviteExpiry::FirstUse;
        assert!(inv.is_valid(t + 1_000_000));
        inv.uses = 1;
        assert!(!inv.is_valid(t));
        inv.expires = InviteExpiry::Never;
        assert!(inv.is_valid(t));
        inv.revoked = true;
        assert!(!inv.is_valid(t));
    }

    #[test]
    fn members_copy_hides_invites() {
        let mut n = net();
        n.invites
            .push(Invite::new(InviteExpiry::Never, Approval::Auto));
        n.pending = true;
        n.join_requests.push(n.members[0].clone());
        let m = n.for_members();
        assert!(m.invites.is_empty() && m.shared_invite.is_none() && !m.pending);
        assert!(m.join_requests.is_empty());
        n.policy.members_invite = true;
        assert_eq!(n.for_members().shared_invite, n.invite_code());
        let json = serde_json::to_string(&n.for_members()).unwrap();
        assert!(
            !json.contains("invites") && !json.contains("pending"),
            "{json}"
        );
    }

    #[test]
    fn network_loads_with_defaults() {
        let n: Network =
            serde_json::from_str(r#"{"id":"ab","name":"x","owner":"o","color":"chartreuse"}"#)
                .unwrap();
        assert_eq!(n.color, NetworkColor::Gray);
        assert!(n.policy.members_share && !n.policy.members_invite);
        assert!(n.members.is_empty() && n.active());
    }
}
