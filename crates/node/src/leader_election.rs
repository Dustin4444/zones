//! Crash-fault leader election for multi-sequencer Zones.
//!
//! Raft is deliberately used only as a replicated fencing token. Zone blocks,
//! acknowledgements, and settlement attestations continue over their existing
//! Commonware channels and are not appended to the Raft log.

use std::{collections::HashMap, time::Duration};

use protobuf::Message as _;
use raft::{Config, RawNode, StateRole, storage::MemStorage};
use slog::{Logger, o};
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};
use zone_p2p::{P2pCommand, P2pPeerId, RaftMessage};

const TICK_INTERVAL: Duration = Duration::from_millis(250);
const HEARTBEAT_TICKS: usize = 2;
const ELECTION_TICKS: usize = 12;

/// Current Raft term and elected node. Consumers must fence block production
/// whenever `local_is_leader` is false.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Leadership {
    pub(crate) term: u64,
    pub(crate) leader_id: Option<u64>,
    pub(crate) local_is_leader: bool,
}

pub(crate) fn spawn_leader_election(
    task_executor: &reth_tasks::TaskExecutor,
    local_id: u64,
    members: Vec<(u64, P2pPeerId)>,
    commands: mpsc::Sender<P2pCommand>,
    messages: mpsc::Receiver<RaftMessage>,
) -> watch::Receiver<Leadership> {
    let (leadership_tx, leadership_rx) = watch::channel(Leadership::default());
    task_executor.spawn_critical_task(
        "zone-raft-leader-election",
        run_leader_election(local_id, members, commands, messages, leadership_tx),
    );
    leadership_rx
}

async fn run_leader_election(
    local_id: u64,
    members: Vec<(u64, P2pPeerId)>,
    commands: mpsc::Sender<P2pCommand>,
    mut messages: mpsc::Receiver<RaftMessage>,
    leadership: watch::Sender<Leadership>,
) {
    let peers_by_id = members.iter().cloned().collect::<HashMap<_, _>>();
    let ids_by_peer = members
        .iter()
        .map(|(id, peer)| (peer.clone(), *id))
        .collect::<HashMap<_, _>>();
    let voter_ids = members.iter().map(|(id, _)| *id).collect::<Vec<_>>();

    let config = Config {
        id: local_id,
        election_tick: ELECTION_TICKS,
        heartbeat_tick: HEARTBEAT_TICKS,
        check_quorum: true,
        pre_vote: true,
        ..Default::default()
    };
    if let Err(err) = config.validate() {
        panic!("invalid Zone Raft configuration: {err}");
    }

    // This spike uses the library's in-memory storage. Production wiring must
    // persist HardState and log entries on the sequencer PVC before enabling
    // automatic failover across process restarts.
    let storage = MemStorage::new_with_conf_state((voter_ids, vec![]));
    let logger = Logger::root(slog::Discard, o!());
    let mut node = RawNode::new(&config, storage, &logger)
        .unwrap_or_else(|err| panic!("failed starting Zone Raft node: {err}"));
    let mut tick = tokio::time::interval(TICK_INTERVAL);
    let mut last = Leadership::default();

    loop {
        tokio::select! {
            _ = tick.tick() => {
                node.tick();
            },
            Some(frame) = messages.recv() => {
                let Some(expected_id) = ids_by_peer.get(&frame.peer).copied() else {
                    warn!(target: "zone::raft", peer = %frame.peer, "Ignoring Raft message from non-member");
                    continue;
                };
                let Ok(message) = raft::eraftpb::Message::parse_from_bytes(&frame.message) else {
                    warn!(target: "zone::raft", peer = %frame.peer, "Ignoring malformed Raft message");
                    continue;
                };
                if message.from != expected_id {
                    warn!(target: "zone::raft", peer = %frame.peer, claimed = message.from, expected = expected_id, "Ignoring Raft message with mismatched authenticated sender");
                    continue;
                }
                if let Err(err) = node.step(message) {
                    warn!(target: "zone::raft", %err, "Rejected Raft protocol message");
                }
            }
            else => return,
        }

        if let Err(err) = process_ready(&mut node, &peers_by_id, &commands).await {
            panic!("Zone Raft ready processing failed: {err}");
        }

        let status = node.status();
        let current = Leadership {
            term: status.hs.term,
            leader_id: (status.ss.leader_id != 0).then_some(status.ss.leader_id),
            local_is_leader: status.ss.raft_state == StateRole::Leader,
        };
        if current != last {
            info!(target: "zone::raft", local_id, term = current.term, leader_id = ?current.leader_id, is_leader = current.local_is_leader, "Raft leadership changed");
            leadership.send_replace(current);
            last = current;
        }
    }
}

async fn process_ready(
    node: &mut RawNode<MemStorage>,
    peers: &HashMap<u64, P2pPeerId>,
    commands: &mpsc::Sender<P2pCommand>,
) -> eyre::Result<()> {
    if !node.has_ready() {
        return Ok(());
    }

    let mut ready = node.ready();
    send_messages(ready.take_messages(), peers, commands).await?;

    if !ready.snapshot().is_empty() {
        node.mut_store()
            .wl()
            .apply_snapshot(ready.snapshot().clone())?;
    }
    if let Some(hard_state) = ready.hs() {
        node.mut_store().wl().set_hardstate(hard_state.clone());
    }
    if !ready.entries().is_empty() {
        node.mut_store().wl().append(ready.entries())?;
    }
    send_messages(ready.take_persisted_messages(), peers, commands).await?;

    let mut light_ready = node.advance(ready);
    send_messages(light_ready.take_messages(), peers, commands).await?;
    node.advance_apply();
    Ok(())
}

async fn send_messages(
    messages: Vec<raft::eraftpb::Message>,
    peers: &HashMap<u64, P2pPeerId>,
    commands: &mpsc::Sender<P2pCommand>,
) -> eyre::Result<()> {
    for message in messages {
        let Some(peer) = peers.get(&message.to).cloned() else {
            eyre::bail!("Raft produced message for unknown node {}", message.to);
        };
        commands
            .send(P2pCommand::SendRaftMessage {
                peer,
                message: message.write_to_bytes()?,
            })
            .await
            .map_err(|_| eyre::eyre!("P2P command channel closed"))?;
    }
    Ok(())
}
