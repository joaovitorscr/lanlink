//! Networks on a running node: the management calls (create, invite, join, approve, remove,
//! leave, delete) and the network control messages. The model is in network.rs, the
//! messages are documented in protocol.rs.
//!
//! Every membership change goes the same way: the owner edits its config, pushes the new
//! `NetworkState` to the members (and to whoever was removed), then `networks_changed`
//! applies the effects locally: peers no longer allowed lose their connections and tunnels,
//! new ones get dialed, and service lists are re-sent because visibility may have changed.
//! Members do the same when they apply a state from the owner.

use crate::api::PeerRequest;
use crate::network::{now_secs, Approval, Invite, InviteCode, InviteExpiry, Member, Network};
use crate::node::{
    sanitize_name, ControlSend, NodeEvent, CLOSE_JOINED, DIAL_TIMEOUT, HANDSHAKE_TIMEOUT,
};
use crate::protocol::{read_msg, write_msg, ControlMsg, JoinStatus};
use crate::{Config, NetworkColor, NetworkPolicy, Node, NodeId, Service, ALPN};
use anyhow::{bail, Context};
use iroh::endpoint::{Connection, SendStream};
use std::collections::HashSet;
use std::time::{Duration, UNIX_EPOCH};
use tokio::task::JoinSet;

/// Members accepted from a `NetworkState`; more are ignored.
const MAX_MEMBERS: usize = 256;
/// Peers that lost access keep their connection this long, so the update telling them
/// (sent just before) reaches them.
const REVOKE_GRACE: Duration = Duration::from_secs(1);
const SEND_TIMEOUT: Duration = Duration::from_secs(3);

/// Whether `peer` may connect to us (`dial` false), or is one we keep connected to (`dial`
/// true). Owners of networks we wait to join or are leaving may connect, but are not dialed.
pub(crate) fn allowed(c: &Config, me: &NodeId, peer: &NodeId, dial: bool) -> bool {
    if peer == me {
        return false;
    }
    let key = peer.to_string();
    if c.allowed_peers.iter().any(|a| a == &key) {
        return true;
    }
    c.networks.iter().any(|n| {
        if n.active() {
            n.has_member(me) && n.has_member(peer)
        } else {
            !dial && n.owner == key
        }
    })
}

/// Every peer for which [`allowed`] is true.
pub(crate) fn peer_set(c: &Config, me: &NodeId, dial: bool) -> HashSet<NodeId> {
    let mut out: HashSet<NodeId> = c
        .allowed_peers
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect();
    for n in &c.networks {
        if n.active() {
            if n.has_member(me) {
                out.extend(n.member_ids());
            }
        } else if !dial {
            out.extend(n.owner_id());
        }
    }
    out.remove(me);
    out
}

/// The enabled services `peer` may see. A service with no networks is visible to direct
/// peers and to members of any shared network; one with networks only to members of those.
/// Networks where only the owner may share count only when we are the owner. In the result
/// `networks` holds just the networks shared with `peer`.
pub(crate) fn visible_services(c: &Config, me: &NodeId, peer: &NodeId) -> Vec<Service> {
    if peer == me {
        return Vec::new();
    }
    let key = peer.to_string();
    let direct = c.allowed_peers.iter().any(|a| a == &key);
    let nets: Vec<&str> = c
        .networks
        .iter()
        .filter(|n| {
            n.active()
                && n.has_member(me)
                && n.has_member(peer)
                && (n.is_owner(me) || n.policy.members_share)
        })
        .map(|n| n.id.as_str())
        .collect();
    c.services
        .iter()
        .filter(|s| s.enabled)
        .filter_map(|s| {
            if s.networks.is_empty() {
                return (direct || !nets.is_empty()).then(|| s.clone());
            }
            let shared: Vec<String> = s
                .networks
                .iter()
                .filter(|id| nets.contains(&id.as_str()))
                .cloned()
                .collect();
            (!shared.is_empty()).then(|| Service {
                networks: shared,
                ..s.clone()
            })
        })
        .collect()
}

/// Remove a network from the config. Services shared only with it are paused rather than
/// becoming visible to everyone.
fn drop_network(c: &mut Config, id: &str) {
    c.networks.retain(|n| n.id != id);
    for s in &mut c.services {
        if s.networks.iter().any(|n| n == id) {
            s.networks.retain(|n| n != id);
            if s.networks.is_empty() {
                s.enabled = false;
            }
        }
    }
}

/// Make a network received from a peer safe to store: bounded, printable, no local or
/// owner-only fields.
fn clean_incoming(mut n: Network) -> Network {
    n.name = sanitize_name(Some(n.name)).unwrap_or_else(|| "Network".into());
    let mut seen = HashSet::new();
    n.members
        .retain(|m| m.id.parse::<NodeId>().is_ok() && seen.insert(m.id.clone()));
    n.members.truncate(MAX_MEMBERS);
    for m in &mut n.members {
        m.name = sanitize_name(Some(std::mem::take(&mut m.name))).unwrap_or_default();
    }
    n.invites.clear();
    n.join_requests.clear();
    n.shared_invite = n
        .shared_invite
        .filter(|c| c.len() <= 256 && InviteCode::parse(c).is_ok());
    n.pending = false;
    n.leaving = false;
    n
}

/// Outcome of a join request on the owner's side.
enum Join {
    /// The network as members see it; true when the joiner was added just now.
    Approved(Network, bool),
    /// Name and owner only; true when the request is new.
    Pending(Network, bool),
    Rejected(String),
}

impl Node {
    pub(crate) fn dial_set(&self) -> HashSet<NodeId> {
        peer_set(&self.inner.config.read().unwrap(), &self.id(), true)
    }

    pub(crate) fn allowed_set(&self) -> HashSet<NodeId> {
        peer_set(&self.inner.config.read().unwrap(), &self.id(), false)
    }

    // ---- management API ----

    /// Networks we own or are in (also pending joins and unconfirmed leaves; see
    /// `Network::active`).
    pub fn networks(&self) -> Vec<Network> {
        self.config().networks
    }

    fn network(&self, id: &str) -> anyhow::Result<Network> {
        self.inner
            .config
            .read()
            .unwrap()
            .networks
            .iter()
            .find(|n| n.id == id)
            .cloned()
            .context("No such network")
    }

    /// Create a network we own, optionally with a first invite code. Saves config.
    pub async fn create_network(
        &self,
        name: &str,
        color: NetworkColor,
        policy: NetworkPolicy,
        invite: Option<(InviteExpiry, Approval)>,
    ) -> anyhow::Result<Network> {
        let name = sanitize_name(Some(name.to_string())).context("Give the network a name")?;
        let mut n = Network::new(name, self.id(), self.our_name());
        n.color = color;
        n.policy = policy;
        if let Some((expires, approval)) = invite {
            n.invites.push(Invite::new(expires, approval));
        }
        self.edit_config(|c| c.networks.push(n.clone()))?;
        tracing::info!(network = %n.id, name = %n.name, "network created");
        self.emit(NodeEvent::NetworksChanged);
        Ok(n)
    }

    /// Apply `f` to a network we own, save, push the new state to its members and to
    /// `extra` (e.g. someone just removed), then apply the effects locally.
    async fn edit_owned(
        &self,
        id: &str,
        extra: &[NodeId],
        f: impl FnOnce(&mut Network) -> anyhow::Result<()>,
    ) -> anyhow::Result<Network> {
        let me = self.id();
        let before = self.allowed_set();
        let mut res = Err(anyhow::anyhow!("No such network"));
        self.edit_config_if(|c| {
            let Some(n) = c.networks.iter_mut().find(|n| n.id == id) else {
                return false;
            };
            if !n.is_owner(&me) {
                res = Err(anyhow::anyhow!(
                    "Only the owner of {} can change it",
                    n.name
                ));
                return false;
            }
            let mut copy = n.clone();
            res = f(&mut copy).map(|()| copy.clone());
            if res.is_ok() {
                *n = copy;
            }
            res.is_ok()
        })?;
        let net = res?;
        self.push_network(&net, extra).await;
        self.networks_changed(before);
        Ok(net)
    }

    /// Rename a network we own. Members get the new name.
    pub async fn rename_network(&self, id: &str, name: &str) -> anyhow::Result<()> {
        let name = sanitize_name(Some(name.to_string())).context("Give the network a name")?;
        self.edit_owned(id, &[], |n| {
            n.name = name;
            Ok(())
        })
        .await
        .map(|_| ())
    }

    /// Change the color or policies of a network we own.
    pub async fn set_network_options(
        &self,
        id: &str,
        color: NetworkColor,
        policy: NetworkPolicy,
    ) -> anyhow::Result<()> {
        self.edit_owned(id, &[], |n| {
            n.color = color;
            n.policy = policy;
            Ok(())
        })
        .await
        .map(|_| ())
    }

    /// Make a new invite code for a network we own. Earlier codes stop working. Returns the code.
    pub async fn create_invite(
        &self,
        id: &str,
        expires: InviteExpiry,
        approval: Approval,
    ) -> anyhow::Result<String> {
        let net = self
            .edit_owned(id, &[], |n| {
                n.invites = vec![Invite::new(expires, approval)];
                Ok(())
            })
            .await?;
        net.invites
            .last()
            .and_then(|i| net.code_for(&i.token))
            .context("invalid owner id")
    }

    /// Change expiry and approval of the current invite code (a new code if there is none).
    /// Returns the code.
    pub async fn update_invite(
        &self,
        id: &str,
        expires: InviteExpiry,
        approval: Approval,
    ) -> anyhow::Result<String> {
        let net = self
            .edit_owned(id, &[], |n| {
                match n.invites.iter_mut().rev().find(|i| !i.revoked) {
                    Some(i) => {
                        i.expires = expires;
                        i.approval = approval;
                        // "After first use" starts counting now.
                        if expires == InviteExpiry::FirstUse {
                            i.uses = 0;
                        }
                    }
                    None => n.invites.push(Invite::new(expires, approval)),
                }
                Ok(())
            })
            .await?;
        net.invite_code()
            .context("The invite code has expired. Make a new one.")
    }

    /// Revoke every invite code of a network we own.
    pub async fn revoke_invites(&self, id: &str) -> anyhow::Result<()> {
        self.edit_owned(id, &[], |n| {
            for i in &mut n.invites {
                i.revoked = true;
            }
            Ok(())
        })
        .await
        .map(|_| ())
    }

    /// Add a member of another network we own to `id` without an invite (used by
    /// `move_member`). Anyone else needs an invite code: members only accept a network they
    /// did not ask to join from an owner whose network they are already in.
    pub async fn add_member(
        &self,
        id: &str,
        peer: NodeId,
        name: Option<String>,
    ) -> anyhow::Result<()> {
        let me = self.id();
        let known = self
            .networks()
            .iter()
            .any(|n| n.id != id && n.active() && n.is_owner(&me) && n.has_member(&peer));
        anyhow::ensure!(
            known,
            "Only members of another network you own can be added directly. Send an invite code instead."
        );
        let key = peer.to_string();
        self.edit_owned(id, &[], |n| {
            if !n.has_member(&peer) {
                n.members.push(Member {
                    id: key.clone(),
                    name: name.unwrap_or_default(),
                    joined_at: now_secs(),
                });
            }
            n.join_requests.retain(|r| r.id != key);
            Ok(())
        })
        .await
        .map(|_| ())
    }

    /// Remove someone from a network we own. They lose access to everything shared in it and
    /// their tunnels for it close.
    pub async fn remove_member(&self, id: &str, peer: NodeId) -> anyhow::Result<()> {
        anyhow::ensure!(
            peer != self.id(),
            "You own this network. Delete it instead."
        );
        let key = peer.to_string();
        self.edit_owned(id, &[peer], |n| {
            anyhow::ensure!(n.has_member(&peer), "They are not in {}", n.name);
            n.members.retain(|m| m.id != key);
            Ok(())
        })
        .await
        .map(|_| ())
    }

    /// Move a member from one network we own to another.
    pub async fn move_member(&self, from: &str, to: &str, peer: NodeId) -> anyhow::Result<()> {
        let key = peer.to_string();
        let name = self
            .network(from)?
            .members
            .into_iter()
            .find(|m| m.id == key)
            .map(|m| m.name)
            .filter(|n| !n.is_empty());
        self.add_member(to, peer, name).await?;
        self.remove_member(from, peer).await
    }

    /// Delete a network we own. Every member is told and loses access.
    pub async fn delete_network(&self, id: &str) -> anyhow::Result<()> {
        let net = self.network(id)?;
        anyhow::ensure!(
            net.is_owner(&self.id()),
            "Only the owner can delete {}. Leave it instead.",
            net.name
        );
        let before = self.allowed_set();
        self.edit_config(|c| drop_network(c, id))?;
        let tombstone = Network {
            members: Vec::new(),
            ..net.for_members()
        };
        self.send_network(tombstone, net.member_ids().collect())
            .await;
        tracing::info!(network = %id, "network deleted");
        self.networks_changed(before);
        Ok(())
    }

    /// Leave a network (or cancel a pending join). The owner is told now, or the next time
    /// we talk to it; until then the network is kept as `leaving` but gives no access.
    pub async fn leave_network(&self, id: &str) -> anyhow::Result<()> {
        let net = self.network(id)?;
        anyhow::ensure!(
            !net.is_owner(&self.id()),
            "You own {}. Delete it instead.",
            net.name
        );
        let before = self.allowed_set();
        self.edit_config(|c| {
            if net.pending {
                drop_network(c, id);
            } else if let Some(n) = c.networks.iter_mut().find(|n| n.id == id) {
                n.leaving = true;
            }
        })?;
        self.networks_changed(before);
        if let (false, Some(owner)) = (net.pending, net.owner_id()) {
            let n = self.clone();
            let msg = ControlMsg::NetworkLeave {
                network: id.to_string(),
            };
            tokio::spawn(async move {
                if !n.send_control(owner, &msg).await {
                    // Not connected: one dial; the handshake repeats the leave.
                    let _ = tokio::time::timeout(DIAL_TIMEOUT, n.ensure_conn(owner)).await;
                }
            });
        }
        Ok(())
    }

    /// Join with an invite code. Approved: we are in, and start connecting to the members.
    /// Pending: the owner has to approve; the network is kept with `pending` set until then.
    /// Errors explain in plain words why it failed (rejected, owner offline, ...).
    pub async fn join_network(
        &self,
        code: &str,
        name: Option<String>,
    ) -> anyhow::Result<JoinStatus> {
        let code = InviteCode::parse(code)?;
        let me = self.id();
        anyhow::ensure!(code.owner != me, "This is an invite to your own network.");
        let joined = self
            .network(&code.network)
            .is_ok_and(|n| n.active() && n.has_member(&me));
        if joined {
            return Ok(JoinStatus::Approved);
        }
        let name = name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| self.our_name());
        let ask = async {
            let conn = self.inner.endpoint.connect(code.owner, ALPN).await?;
            let (mut send, mut recv) = conn.open_bi().await?;
            let req = ControlMsg::JoinRequest {
                network: code.network.clone(),
                token: code.token.clone(),
                name: Some(name),
            };
            write_msg(&mut send, &req).await?;
            let resp: ControlMsg = read_msg(&mut recv).await?;
            conn.close(CLOSE_JOINED.into(), b"done");
            anyhow::Ok(resp)
        };
        let resp = match tokio::time::timeout(DIAL_TIMEOUT + HANDSHAKE_TIMEOUT, ask).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                tracing::info!("join request failed: {e:#}");
                bail!("Could not reach the owner of this network. They may be offline; try again later.")
            }
            Err(_) => {
                bail!("The owner of this network did not answer. They may be offline; try again later.")
            }
        };
        let ControlMsg::JoinResponse {
            network,
            status,
            state,
            reason,
        } = resp
        else {
            bail!("The owner's lanlink is too old for networks. Ask them to update.");
        };
        anyhow::ensure!(
            network == code.network,
            "The owner sent an unexpected answer"
        );
        let owner = code.owner.to_string();
        let state = state
            .filter(|n| n.id == code.network && n.owner == owner)
            .map(clean_incoming);
        let before = self.allowed_set();
        match status {
            JoinStatus::Approved => {
                let net = state
                    .filter(|n| n.has_member(&me))
                    .context("The owner sent an unexpected answer")?;
                tracing::info!(network = %net.id, name = %net.name, "joined network");
                self.edit_config(|c| {
                    c.networks.retain(|n| n.id != net.id);
                    c.networks.push(net);
                })?;
            }
            JoinStatus::Pending => {
                let mut net = state.context("The owner sent an unexpected answer")?;
                net.members.clear();
                net.pending = true;
                tracing::info!(network = %net.id, "join request waits for approval");
                self.edit_config(|c| {
                    c.networks.retain(|n| n.id != net.id);
                    c.networks.push(net);
                })?;
            }
            JoinStatus::Rejected | JoinStatus::Unknown => {
                bail!(reason.unwrap_or_else(|| "The owner refused this invite code.".into()))
            }
        }
        self.networks_changed(before);
        Ok(status)
    }

    /// Answer a join request to a network we own. Allow adds them as a member.
    pub async fn respond_join(
        &self,
        network: &str,
        peer: NodeId,
        allow: bool,
    ) -> anyhow::Result<()> {
        let key = peer.to_string();
        self.edit_owned(network, &[], |n| {
            let pos = n
                .join_requests
                .iter()
                .position(|r| r.id == key)
                .context("No such join request")?;
            let mut m = n.join_requests.remove(pos);
            if allow && !n.has_member(&peer) {
                m.joined_at = now_secs();
                n.members.push(m);
            }
            Ok(())
        })
        .await
        .map(|_| ())
    }

    /// Open join requests to networks we own, oldest first.
    pub(crate) fn join_requests(&self) -> Vec<PeerRequest> {
        let me = self.id();
        let c = self.inner.config.read().unwrap();
        c.networks
            .iter()
            .filter(|n| n.is_owner(&me))
            .flat_map(|n| {
                n.join_requests.iter().filter_map(|r| {
                    Some(PeerRequest {
                        id: r.id.parse().ok()?,
                        name: Some(r.name.clone()).filter(|s| !s.is_empty()),
                        at: UNIX_EPOCH + Duration::from_secs(r.joined_at),
                        network: Some(n.id.clone()),
                    })
                })
            })
            .collect()
    }

    // ---- effects ----

    /// The allowed set changed from `before`: drop peers that lost access (connections,
    /// tunnels, saved tunnels, requests), dial new ones, re-send service lists.
    pub(crate) fn networks_changed(&self, before: HashSet<NodeId>) {
        let after = self.allowed_set();
        let gone: Vec<NodeId> = before.difference(&after).copied().collect();
        if !gone.is_empty() {
            tracing::info!(
                count = gone.len(),
                "peers lost access through a network change"
            );
            for id in &gone {
                self.stop_dialer(*id);
                self.close_peer_tunnels(*id);
            }
            let keys: Vec<String> = gone.iter().map(|id| id.to_string()).collect();
            let res = self.edit_config_if(|c| {
                let n = c.saved_tunnels.len();
                c.saved_tunnels.retain(|t| !keys.contains(&t.peer));
                c.saved_tunnels.len() != n
            });
            if let Err(e) = res {
                tracing::warn!("forgetting saved tunnels: {e:#}");
            }
            let n = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(REVOKE_GRACE).await;
                for id in gone {
                    if !n.is_allowed(&id) {
                        n.disconnect_peer(id);
                        n.inner.peers.lock().unwrap().remove(&id);
                    }
                }
            });
        }
        self.sync_dialers();
        self.push_services();
        self.emit(NodeEvent::NetworksChanged);
    }

    // ---- wire ----

    /// Write `msg` on a control stream to `peer`. False when not connected or it failed.
    pub(crate) async fn send_control(&self, peer: NodeId, msg: &ControlMsg) -> bool {
        let controls: Vec<ControlSend> = self
            .inner
            .peers
            .lock()
            .unwrap()
            .get(&peer)
            .map(|e| e.controls.iter().map(|(_, s)| s.clone()).collect())
            .unwrap_or_default();
        for s in controls {
            let res = tokio::time::timeout(SEND_TIMEOUT, async {
                write_msg(&mut *s.lock().await, msg).await
            })
            .await;
            if matches!(res, Ok(Ok(()))) {
                return true;
            }
        }
        false
    }

    async fn send_network(&self, network: Network, to: Vec<NodeId>) {
        let me = self.id();
        let msg = ControlMsg::NetworkState { network };
        let mut set = JoinSet::new();
        for id in to.into_iter().filter(|id| *id != me) {
            let n = self.clone();
            let msg = msg.clone();
            set.spawn(async move { n.send_control(id, &msg).await });
        }
        while set.join_next().await.is_some() {}
    }

    /// Send the members' view of `net` to its connected members and to `extra`.
    async fn push_network(&self, net: &Network, extra: &[NodeId]) {
        let mut to: Vec<NodeId> = net.member_ids().collect();
        for id in extra {
            if !to.contains(id) {
                to.push(*id);
            }
        }
        self.send_network(net.for_members(), to).await;
    }

    /// Right after a handshake: the complete list of networks we own that `peer` is in, and
    /// any leave from us it has not confirmed.
    pub(crate) fn send_network_sync(&self, peer: NodeId, send: &ControlSend) {
        let me = self.id();
        let key = peer.to_string();
        let (networks, leaves): (Vec<Network>, Vec<String>) = {
            let c = self.inner.config.read().unwrap();
            (
                c.networks
                    .iter()
                    .filter(|n| n.active() && n.is_owner(&me) && n.has_member(&peer))
                    .map(Network::for_members)
                    .collect(),
                c.networks
                    .iter()
                    .filter(|n| n.leaving && n.owner == key)
                    .map(|n| n.id.clone())
                    .collect(),
            )
        };
        let send = send.clone();
        tokio::spawn(async move {
            let mut s = send.lock().await;
            let _ = write_msg(&mut *s, &ControlMsg::Networks { networks }).await;
            for network in leaves {
                let _ = write_msg(&mut *s, &ControlMsg::NetworkLeave { network }).await;
            }
        });
    }

    pub(crate) fn apply_network_state(&self, from: NodeId, network: Network) {
        self.apply_networks(from, vec![network], false);
    }

    pub(crate) fn apply_network_sync(&self, from: NodeId, networks: Vec<Network>) {
        self.apply_networks(from, networks, true);
    }

    /// Networks pushed by `from`. Only those it owns are considered. With `complete`, cached
    /// networks owned by `from` that are missing were left behind (we were removed, or it was
    /// deleted) and are dropped.
    fn apply_networks(&self, from: NodeId, networks: Vec<Network>, complete: bool) {
        let me = self.id();
        let owner = from.to_string();
        let networks: Vec<Network> = networks
            .into_iter()
            .filter(|n| n.owner == owner)
            .map(clean_incoming)
            .collect();
        let before = self.allowed_set();
        let mut resend_leave = Vec::new();
        let mut refused = Vec::new();
        let res = self.edit_config_if(|c| {
            // A network we did not ask to join is only taken from an owner whose network we
            // are already in (a "Move to" between two of its networks). Otherwise any allowed
            // peer could make up a network with us in it and make strangers allowed.
            let trusted_owner = c
                .networks
                .iter()
                .any(|n| n.owner == owner && n.active() && n.has_member(&me));
            let mut changed = false;
            for net in &networks {
                let ours = net.has_member(&me);
                match c.networks.iter().position(|n| n.id == net.id) {
                    // Someone else's id: ignore.
                    Some(i) if c.networks[i].owner != owner => {}
                    Some(i) if c.networks[i].leaving && ours => resend_leave.push(net.id.clone()),
                    Some(i) if ours => {
                        if c.networks[i] != *net {
                            c.networks[i] = net.clone();
                            changed = true;
                        }
                    }
                    Some(_) => {
                        drop_network(c, &net.id);
                        changed = true;
                    }
                    None if ours && trusted_owner => {
                        c.networks.push(net.clone());
                        changed = true;
                    }
                    None if ours => refused.push(net.id.clone()),
                    None => {}
                }
            }
            if complete {
                let gone: Vec<String> = c
                    .networks
                    .iter()
                    .filter(|n| {
                        n.owner == owner && !n.pending && !networks.iter().any(|x| x.id == n.id)
                    })
                    .map(|n| n.id.clone())
                    .collect();
                for id in &gone {
                    drop_network(c, id);
                    changed = true;
                }
            }
            changed
        });
        for id in refused {
            tracing::info!(%from, network = %id, "ignoring a network we did not ask to join");
        }
        match res {
            Ok(true) => {
                tracing::info!(%from, "networks updated by their owner");
                self.networks_changed(before);
            }
            Ok(false) => {}
            Err(e) => tracing::warn!("saving networks: {e:#}"),
        }
        for network in resend_leave {
            let n = self.clone();
            tokio::spawn(async move {
                n.send_control(from, &ControlMsg::NetworkLeave { network })
                    .await;
            });
        }
    }

    /// The owner of networks we are in refused our connection, so it no longer counts us as
    /// a member (we were removed, or it deleted the network, while we were offline).
    pub(crate) fn owner_rejected(&self, owner: NodeId) {
        let key = owner.to_string();
        let before = self.allowed_set();
        let res = self.edit_config_if(|c| {
            let ids: Vec<String> = c
                .networks
                .iter()
                .filter(|n| n.owner == key && !n.pending)
                .map(|n| n.id.clone())
                .collect();
            for id in &ids {
                drop_network(c, id);
            }
            !ids.is_empty()
        });
        if let Ok(true) = res {
            tracing::info!(%owner, "network owner no longer knows us; dropped its networks");
            self.networks_changed(before);
        }
    }

    /// A member asked to leave a network we own.
    pub(crate) async fn handle_leave(&self, from: NodeId, id: &str) {
        let me = self.id();
        let ok = self
            .network(id)
            .is_ok_and(|n| n.is_owner(&me) && n.has_member(&from));
        if !ok || from == me {
            return;
        }
        let key = from.to_string();
        tracing::info!(%from, network = %id, "member left");
        let res = self
            .edit_owned(id, &[from], |n| {
                n.members.retain(|m| m.id != key);
                Ok(())
            })
            .await;
        if let Err(e) = res {
            tracing::warn!("removing a member that left: {e:#}");
        }
    }

    /// Owner side of a join: answer, close, and apply the result.
    pub(crate) async fn handle_join(
        &self,
        conn: Connection,
        mut send: SendStream,
        network: String,
        token: String,
        name: Option<String>,
    ) {
        let peer = conn.remote_id();
        let before = self.allowed_set();
        let outcome = self.process_join(peer, &network, &token, sanitize_name(name.clone()));
        let (status, state, reason) = match &outcome {
            Join::Approved(n, _) => (JoinStatus::Approved, Some(n.clone()), None),
            Join::Pending(n, _) => (JoinStatus::Pending, Some(n.clone()), None),
            Join::Rejected(r) => (JoinStatus::Rejected, None, Some(r.clone())),
        };
        tracing::info!(%peer, %network, ?status, "join request");
        let msg = ControlMsg::JoinResponse {
            network,
            status,
            state,
            reason,
        };
        let _ = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
            write_msg(&mut send, &msg).await?;
            send.finish()?;
            anyhow::Ok(())
        })
        .await;
        match outcome {
            Join::Approved(net, true) => {
                self.push_network(&net, &[]).await;
                self.networks_changed(before);
            }
            Join::Pending(net, true) => {
                let req = self
                    .join_requests()
                    .into_iter()
                    .find(|r| r.id == peer && r.network.as_deref() == Some(&net.id));
                if let Some(req) = req {
                    self.emit(NodeEvent::PeerRequest(req));
                }
            }
            _ => {}
        }
        // The joiner closes once it has read the answer.
        let _ = tokio::time::timeout(HANDSHAKE_TIMEOUT, conn.closed()).await;
        conn.close(CLOSE_JOINED.into(), b"answered");
    }

    fn process_join(&self, peer: NodeId, network: &str, token: &str, name: Option<String>) -> Join {
        let me = self.id();
        let key = peer.to_string();
        let now = now_secs();
        let invalid = || {
            Join::Rejected(
                "This invite code is no longer valid (expired or revoked). Ask for a new one."
                    .into(),
            )
        };
        let stub = |n: &Network| Network {
            members: Vec::new(),
            ..n.for_members()
        };
        // Check before editing, so bad tokens never touch the disk.
        let Ok(net) = self.network(network) else {
            return Join::Rejected("This network does not exist anymore.".into());
        };
        if !net.is_owner(&me) || !net.active() {
            return Join::Rejected("This network does not exist anymore.".into());
        }
        if net.has_member(&peer) {
            return Join::Approved(net.for_members(), false);
        }
        if net.join_requests.iter().any(|r| r.id == key) {
            return Join::Pending(stub(&net), false);
        }
        if !net
            .invites
            .iter()
            .any(|i| i.token == token && i.is_valid(now))
        {
            return invalid();
        }
        let member = Member {
            id: key,
            name: name.unwrap_or_default(),
            joined_at: now,
        };
        let mut out = invalid();
        let res = self.edit_config_if(|c| {
            let Some(n) = c.networks.iter_mut().find(|n| n.id == network) else {
                return false;
            };
            // Checked again under the edit lock: a "first use" code works only once.
            let Some(inv) = n
                .invites
                .iter_mut()
                .find(|i| i.token == token && i.is_valid(now))
            else {
                return false;
            };
            inv.uses += 1;
            match inv.approval {
                Approval::Auto => {
                    n.members.push(member);
                    out = Join::Approved(n.for_members(), true);
                }
                Approval::AskMe => {
                    n.join_requests.push(member);
                    out = Join::Pending(stub(n), true);
                }
            }
            true
        });
        if let Err(e) = res {
            tracing::warn!("saving join request: {e:#}");
            return Join::Rejected("The owner could not save your request. Try again.".into());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SecretKey;

    fn with_member(mut n: Network, id: NodeId) -> Network {
        n.members.push(Member {
            id: id.to_string(),
            name: String::new(),
            joined_at: 0,
        });
        n
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unsolicited_network_is_ignored() -> anyhow::Result<()> {
        // edit_config saves; keep it away from the user's real config.
        let dir = std::env::temp_dir().join(format!("lanlink-test-{}", std::process::id()));
        std::env::set_var("LANLINK_CONFIG_DIR", &dir);
        let friend = SecretKey::generate().public();
        let stranger = SecretKey::generate().public();
        let config = Config {
            allowed_peers: vec![friend.to_string()],
            disable_lan_detection: true,
            ..Default::default()
        };
        let node = Node::start_with_secret_key(config, SecretKey::generate()).await?;
        let me = node.id();

        // A direct peer makes up a network with us and a stranger in it: ignored.
        let fake = with_member(with_member(Network::new("Fake", friend, "f"), me), stranger);
        node.apply_network_state(friend, fake.clone());
        node.apply_network_sync(friend, vec![fake]);
        assert!(node.networks().is_empty());
        assert!(!node.is_allowed(&stranger));
        assert!(!node.dial_set().contains(&stranger));

        // Once we are in one of its networks, the same owner may add us to another one
        // ("Move to").
        let crew = with_member(Network::new("Crew", friend, "f"), me);
        node.edit_config(|c| c.networks.push(crew))?;
        let other = with_member(
            with_member(Network::new("Other", friend, "f"), me),
            stranger,
        );
        node.apply_network_state(friend, other.clone());
        assert!(node.networks().iter().any(|n| n.id == other.id));
        assert!(node.is_allowed(&stranger));

        // Someone else claiming to own it is ignored.
        let mut hijack = other.clone();
        hijack.owner = stranger.to_string();
        hijack.name = "Mine now".into();
        node.apply_network_state(stranger, hijack);
        let n = node
            .networks()
            .into_iter()
            .find(|n| n.id == other.id)
            .unwrap();
        assert_eq!(n.owner, friend.to_string());

        node.shutdown().await?;
        Ok(())
    }
}
