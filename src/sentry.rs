use std::collections::{HashSet, VecDeque};
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use libp2p::PeerId;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{debug, info};
use types::EthSpec;
use types::ForkContext;
use types::ForkName;
use types::Hash256;
use types::MainnetEthSpec;
use types::{ChainSpec, Epoch, Slot};

use crate::rpc::StatusMessage;
use crate::types::GossipKind;
use crate::{Context, PubsubMessage, Request, Response};
use crate::{Enr, Network};
use crate::{NetworkConfig, NetworkEvent};

#[derive(Debug, Clone)]
pub struct SentryMessage {
    pub peer_id: PeerId,
    pub remote_addr: Option<SocketAddr>,
    pub message: PubsubMessage<MainnetEthSpec>,
}

#[derive(Debug, Clone)]
pub struct SentryConfig {
    pub boot_enrs: Vec<Enr>,
    pub libp2p_port: u16,
    pub discovery_port: u16,
    pub max_peers: usize,
    pub metrics_enabled: bool,
    /// Load profile configuration. Use 6 for low latency but
    /// high bw.
    pub network_load: u8,
}

impl Default for SentryConfig {
    fn default() -> Self {
        Self {
            boot_enrs: bootnode_enrs(),
            libp2p_port: 9000,
            discovery_port: 9000,
            max_peers: 50,
            metrics_enabled: true,
            network_load: 6,
        }
    }
}

#[derive(Debug)]
pub enum SentryCommand {
    Subscribe(GossipKind),
    Publish(PubsubMessage<MainnetEthSpec>),
    PeerCount(oneshot::Sender<usize>),
    LocalEnr(oneshot::Sender<Enr>),
    AddTrusted(PeerId, Enr),
    RemoveTrusted(PeerId),
}

#[derive(Clone, Debug)]
pub struct SentryHandle {
    cmd_tx: UnboundedSender<SentryCommand>,
}

impl SentryHandle {
    pub fn subscribe_topic(&self, gossip_kind: GossipKind) {
        let _ = self.cmd_tx.send(SentryCommand::Subscribe(gossip_kind));
    }

    pub async fn peer_count(&self) -> usize {
        let (tx, rx) = oneshot::channel();
        let _ = self.cmd_tx.send(SentryCommand::PeerCount(tx));
        rx.await.unwrap_or(0)
    }

    pub async fn local_enr(&self) -> Enr {
        let (tx, rx) = oneshot::channel();
        let _ = self.cmd_tx.send(SentryCommand::LocalEnr(tx));
        rx.await.unwrap()
    }

    /// Adds a trusted peer. If the peer is not connected yet
    /// this will trigger a dial. This peer will be exempt from any peer
    /// scoring and will be dialed immediately.
    pub fn add_trusted_peer(&self, peer_id: PeerId, enr: Enr) {
        let _ = self.cmd_tx.send(SentryCommand::AddTrusted(peer_id, enr));
    }

    /// Removes a trusted peer. It does not disconnect the peer but removes
    /// it from the explicit peer set. It also untags it as a trusted peer.
    pub fn remove_trusted_peer(&self, peer_id: PeerId) {
        let _ = self.cmd_tx.send(SentryCommand::RemoveTrusted(peer_id));
    }

    /// Publish a pubsub message.
    pub fn publish(&self, message: PubsubMessage<MainnetEthSpec>) {
        let _ = self.cmd_tx.send(SentryCommand::Publish(message));
    }
}

#[derive(Debug)]
pub struct Sentry {
    cfg: SentryConfig,
    cmd_rx: UnboundedReceiver<SentryCommand>,
    events_tx: Option<UnboundedSender<SentryMessage>>,
    handle: SentryHandle,
}

impl Sentry {
    pub fn new(cfg: SentryConfig) -> Sentry {
        let (tx, rx) = mpsc::unbounded_channel();
        Sentry {
            cfg,
            cmd_rx: rx,
            handle: SentryHandle { cmd_tx: tx },
            events_tx: None,
        }
    }

    /// Returns a clone of the `ServiceHandle` which can be used to send commands to the service.
    pub fn handle(&self) -> SentryHandle {
        self.handle.clone()
    }

    /// Returns a stream of [`PubsubMessage`] events. Note that this will only return events
    /// to which we've subscribed via the [`ServiceHandle`].
    pub fn pubsub_event_stream(&mut self) -> UnboundedReceiverStream<SentryMessage> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.events_tx = Some(tx);
        UnboundedReceiverStream::new(rx)
    }

    pub async fn start(mut self) -> ! {
        // Create the inner `NetworkConfig`
        let network_config = NetworkConfig {
            libp2p_port: self.cfg.libp2p_port,
            discovery_port: self.cfg.discovery_port,
            boot_nodes_enr: self.cfg.boot_enrs,
            target_peers: self.cfg.max_peers,
            metrics_enabled: self.cfg.metrics_enabled,
            network_load: self.cfg.network_load,
            ..Default::default()
        };

        // Specify the fork
        let fork = ForkName::Capella;

        // Populate the chain spec
        let mainnet_spec = ChainSpec::mainnet();

        let capella_slot = mainnet_spec
            .fork_epoch(fork)
            .unwrap()
            .start_slot(MainnetEthSpec::slots_per_epoch());

        // https://eth2book.info/bellatrix/part3/containers/state/#beacon-state
        let genesis_validators_root =
            Hash256::from_str("0x4b363db94e286120d76eb905340fdd4e54bfe9f06bf33ff6cf5ad27f511bfe95")
                .unwrap();

        // Build the merge fork context
        let capella_fork_context = ForkContext::new::<MainnetEthSpec>(
            capella_slot,
            genesis_validators_root,
            &mainnet_spec,
        );

        let fork_digest = capella_fork_context.to_context_bytes(fork).unwrap();
        info!(slot = ?capella_slot, "Fork digest: {:?}", fork_digest);

        // Build the network service context
        let ctx = Context {
            config: &network_config,
            enr_fork_id: mainnet_spec
                .enr_fork_id::<MainnetEthSpec>(capella_slot, genesis_validators_root),
            fork_context: Arc::new(capella_fork_context),
            chain_spec: &mainnet_spec,
            gossipsub_registry: None,
        };

        let (mut network, globals) = Network::<usize, MainnetEthSpec>::new(ctx).await.unwrap();

        // Set a random default status (for now)
        let mut highest_status = StatusMessage {
            fork_digest,
            finalized_root: Hash256::from_str(
                "0xb6adca904a0674b7263f8f9518b2a0dff5ee6089ee92890e742d0a64a2cbbb43",
            )
            .unwrap(),
            finalized_epoch: Epoch::new(194863),
            head_root: Hash256::from_str(
                "0xb41d25d17ef959d15aabdc01df99e2ec94dd600a0ac218d5b79b2a95cb14acad",
            )
            .unwrap(),
            head_slot: Slot::new(6235698),
        };

        let mut epoch_blocks: VecDeque<(Slot, Hash256)> = VecDeque::with_capacity(3);
        let mut epoch_up_to_date = false;
        let mut last_epoch = Epoch::new(0);

        let mut trusted_peers = HashSet::new();

        loop {
            tokio::select! {
                cmd = self.cmd_rx.recv() => {
                    if let Some(cmd) = cmd {
                        match cmd {
                            SentryCommand::Subscribe(kind) => {
                                debug!(?kind, "New topic subscription");
                                network.subscribe_kind(kind);
                            }
                            SentryCommand::Publish(data) => {
                                debug!(kind = ?data.kind(), "Publishing message");
                                network.publish(vec![data]);
                            }
                            SentryCommand::PeerCount(tx) => {
                                let _ = tx.send(globals.connected_peers());
                            }
                            SentryCommand::LocalEnr(tx) => {
                                let _ = tx.send(network.local_enr());
                            }
                            SentryCommand::AddTrusted(peer_id, enr) => {
                                // Add the peer as an explicit peer. This will make sure they will always
                                // receive our pubsub messages immediately.
                                info!(%peer_id, ip = ?enr.ip4(), "Dialing trusted peer");
                                network.add_enr(enr.clone());
                                if !network.peer_manager().is_connected(&peer_id) {
                                    network.peer_manager_mut().dial_trusted_peer(&peer_id, Some(enr));
                                    trusted_peers.insert(peer_id);
                                }
                            }
                            SentryCommand::RemoveTrusted(peer_id) => {
                                // Removes the peer from our trusted peer set
                                if trusted_peers.remove(&peer_id) {
                                    network.gossipsub_mut().remove_explicit_peer(&peer_id);
                                    network.peer_manager_mut().remove_trusted(&peer_id);
                                }
                            }
                        }
                    }
                }
                event = network.next_event() => {
                    match event {
                        NetworkEvent::PeerConnectedIncoming(id) => {
                            debug!(peer = ?id, "Peer connected (incoming)");
                            if trusted_peers.contains(&id) {
                                info!(peer = ?id, "Trusted peer connected (incoming)");
                                network.gossipsub_mut().add_explicit_peer(&id);
                            }
                        }
                        // NOTE: we have to send status messages when connecting here:
                        // https://github.com/ethereum/consensus-specs/blob/dev/specs/phase0/p2p-interface.md#status
                        NetworkEvent::PeerConnectedOutgoing(id) => {
                            debug!(peer = ?id, "Peer connected (outgoing)");
                            network.send_request(id, 10, Request::Status(highest_status.clone()));

                            if trusted_peers.contains(&id) {
                                info!(peer = ?id, "Trusted peer connected (outgoing)");
                                network.gossipsub_mut().add_explicit_peer(&id);
                            }
                        }
                        NetworkEvent::PeerDisconnected(id) => {
                            debug!(peer = ?id, "Peer disconnected");
                        }
                        NetworkEvent::PubsubMessage { message, source, .. } => {
                            if let PubsubMessage::BeaconBlock(ref block) = message {
                                let slot = block.slot();
                                let root = block.canonical_root();

                                debug!(slot = ?slot, root = ?root, "Received block");

                                // Epoch if and only if it was a bigger epoch than the last one
                                if slot % 32 == 0 && Epoch::from(slot.as_u64() / 32) > last_epoch {
                                    last_epoch = Epoch::from(slot.as_u64() / 32);
                                    epoch_blocks.push_back((slot, root));
                                    if epoch_blocks.len() > 2 {
                                        let (finalized_slot, finalized_root) = epoch_blocks.pop_front().unwrap();

                                        highest_status.finalized_root = finalized_root;
                                        highest_status.finalized_epoch = Epoch::new(finalized_slot.as_u64() / 32);
                                        epoch_up_to_date = true;
                                        debug!(?highest_status, "Epoch finalized");
                                    }
                                }

                                if epoch_up_to_date && slot > highest_status.head_slot {
                                    highest_status.head_root = root;
                                    highest_status.head_slot = slot;
                                    debug!(?highest_status, "Updated highest status");
                                }
                            }

                            if let Some(tx) = &self.events_tx {
                                let remote_addr = if let Some(info) = globals.peers.read().peer_info(&source) {
                                    info.seen_addresses().cloned().next()
                                } else {
                                    None
                                };

                                // Ignore errors if the receiver has been dropped
                                let _ = tx.send(SentryMessage { peer_id: source, remote_addr, message });
                            }
                        }
                        NetworkEvent::RequestReceived {
                            peer_id,
                            id,
                            request,
                        } => {
                            match request {
                                Request::Status(status) => {
                                    info!(peer = ?peer_id, ?status, "Received status");
                                    // Respond to status
                                    network.send_response(
                                        peer_id,
                                        id,
                                        Response::Status(highest_status.clone()),
                                    );
                                }
                                _ => {
                                    debug!(peer = ?peer_id, ?request, "Received other request");
                                }
                            }
                        }
                        NetworkEvent::RPCFailed { id, peer_id } => {
                            debug!(peer = ?peer_id, %id, "RPC failed");
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

/// Returns the ENRs of the bootnodes.
pub fn bootnode_enrs() -> Vec<Enr> {
    vec![
        // Lighthouse team
        Enr::from_str("enr:-Jq4QN6_FzIYyfJET9hiLcGUsg_EVOwCQ4bwsBwe0S4ElrfXUXufSYLtQAHU9_LuO9uice7EAaLbDlMK8QEhtyg8Oh4BhGV0aDKQtTA_KgAAAAD__________4JpZIJ2NIJpcIQDGh4giXNlY3AyNTZrMaECSHaY_36GdNjF8-CLfMSg-8lB0wce5VRZ96HkT9tSkVeDdWRwgiMo").unwrap(),
        Enr::from_str("enr:-Jq4QMOjjkLYSN7GVAf_zBSS5c_MokSPMZZvmjLUYiuHrPLHInjeBtF1IfskuYlmhglGan2ECmPk89SRXr4FY1jVp5YBhGV0aDKQtTA_KgAAAAD__________4JpZIJ2NIJpcIQi8wB6iXNlY3AyNTZrMaEC0EiXxAB2QKZJuXnUwmf-KqbP9ZP7m9gsRxcYvoK9iTCDdWRwgiMo").unwrap(),

        // EF
        Enr::from_str("enr:-Ku4QHqVeJ8PPICcWk1vSn_XcSkjOkNiTg6Fmii5j6vUQgvzMc9L1goFnLKgXqBJspJjIsB91LTOleFmyWWrFVATGngBh2F0dG5ldHOIAAAAAAAAAACEZXRoMpC1MD8qAAAAAP__________gmlkgnY0gmlwhAMRHkWJc2VjcDI1NmsxoQKLVXFOhp2uX6jeT0DvvDpPcU8FWMjQdR4wMuORMhpX24N1ZHCCIyg").unwrap(),
        Enr::from_str("enr:-Ku4QG-2_Md3sZIAUebGYT6g0SMskIml77l6yR-M_JXc-UdNHCmHQeOiMLbylPejyJsdAPsTHJyjJB2sYGDLe0dn8uYBh2F0dG5ldHOIAAAAAAAAAACEZXRoMpC1MD8qAAAAAP__________gmlkgnY0gmlwhBLY-NyJc2VjcDI1NmsxoQORcM6e19T1T9gi7jxEZjk_sjVLGFscUNqAY9obgZaxbIN1ZHCCIyg").unwrap(),
        Enr::from_str("enr:-Ku4QPn5eVhcoF1opaFEvg1b6JNFD2rqVkHQ8HApOKK61OIcIXD127bKWgAtbwI7pnxx6cDyk_nI88TrZKQaGMZj0q0Bh2F0dG5ldHOIAAAAAAAAAACEZXRoMpC1MD8qAAAAAP__________gmlkgnY0gmlwhDayLMaJc2VjcDI1NmsxoQK2sBOLGcUb4AwuYzFuAVCaNHA-dy24UuEKkeFNgCVCsIN1ZHCCIyg").unwrap(),

        // Prysmatic
        Enr::from_str("enr:-Ku4QImhMc1z8yCiNJ1TyUxdcfNucje3BGwEHzodEZUan8PherEo4sF7pPHPSIB1NNuSg5fZy7qFsjmUKs2ea1Whi0EBh2F0dG5ldHOIAAAAAAAAAACEZXRoMpD1pf1CAAAAAP__________gmlkgnY0gmlwhBLf22SJc2VjcDI1NmsxoQOVphkDqal4QzPMksc5wnpuC3gvSC8AfbFOnZY_On34wIN1ZHCCIyg").unwrap(),
        Enr::from_str("enr:-Ku4QP2xDnEtUXIjzJ_DhlCRN9SN99RYQPJL92TMlSv7U5C1YnYLjwOQHgZIUXw6c-BvRg2Yc2QsZxxoS_pPRVe0yK8Bh2F0dG5ldHOIAAAAAAAAAACEZXRoMpD1pf1CAAAAAP__________gmlkgnY0gmlwhBLf22SJc2VjcDI1NmsxoQMeFF5GrS7UZpAH2Ly84aLK-TyvH-dRo0JM1i8yygH50YN1ZHCCJxA").unwrap(),
        Enr::from_str("enr:-Ku4QPp9z1W4tAO8Ber_NQierYaOStqhDqQdOPY3bB3jDgkjcbk6YrEnVYIiCBbTxuar3CzS528d2iE7TdJsrL-dEKoBh2F0dG5ldHOIAAAAAAAAAACEZXRoMpD1pf1CAAAAAP__________gmlkgnY0gmlwhBLf22SJc2VjcDI1NmsxoQMw5fqqkw2hHC4F5HZZDPsNmPdB1Gi8JPQK7pRc9XHh-oN1ZHCCKvg").unwrap(),
    ]
}

#[cfg(test)]
mod tests {
    use std::{sync::Once, time::Duration};

    use tokio_stream::StreamExt;
    use tracing_subscriber::EnvFilter;

    use super::*;

    static INIT: Once = Once::new();

    pub fn init_tracing() {
        INIT.call_once(|| {
            let subscriber = tracing_subscriber::fmt()
                .with_env_filter(EnvFilter::from_default_env())
                .finish();

            tracing::subscriber::set_global_default(subscriber)
                .expect("setting default subscriber failed");
        });
    }

    #[tokio::test]
    async fn test_trusted_dial() {
        init_tracing();
        let peer_id =
            PeerId::from_str("16Uiu2HAmFR3ZHBDGPUcFQhBBFotnH6QRg1XEt8eh3zPajD5LK8wv").unwrap();
        let enr = Enr::from_str("enr:-Ly4QEaX8wHWcs725rWF85D99paqIoVdXx6xq9HZLELvAsX6ZFVrHVbUbcOAg3-tBaVVSx_UTkko_eA7XJecUrF4pNsQh2F0dG5ldHOIAAAAAAAAAMCEZXRoMpC7pNqWAwAAAP__________gmlkgnY0gmlwhEFtarOJc2VjcDI1NmsxoQMpAI4oXOjuj6RmJgxgNH7E-NHOArXcE9Bnhx1tQqdi7YhzeW5jbmV0cwCDdGNwgicPg3VkcIInDw").unwrap();
        // let peer_id =
        //     PeerId::from_str("16Uiu2HAmS76cdxj4D44pHy4fhJP6D6QopD5L4vbMVw2Ty3gk8sAd").unwrap();
        // let enr = Enr::from_str("enr:-L64QCyFmg1w2Tu7MJd0Fzkel-maMuT_hlH0-9HOmecC73IuN5T1AOfiNJy1OpUVqUBph0x-5htj51OHAgB_jA71rhqCBy2HYXR0bmV0c4gIAAAAAAAAQIRldGgykLuk2pYDAAAA__________-CaWSCdjSCaXCEVMSGeolzZWNwMjU2azGhA8fYHVCgtn7wAEvtjimNxPzdN48YQf8hKka_SBQaF0veiHN5bmNuZXRzAIN0Y3CCBACDdWRwggQA").unwrap();

        let mut sentry = Sentry::new(SentryConfig {
            boot_enrs: bootnode_enrs(),
            libp2p_port: 9000,
            discovery_port: 9000,
            max_peers: 1,
            metrics_enabled: false,
            network_load: 6,
        });

        let handle = sentry.handle();
        let mut events = sentry.pubsub_event_stream();
        handle.subscribe_topic(GossipKind::BeaconBlock);

        tokio::spawn(sentry.start());

        tokio::time::sleep(Duration::from_millis(100)).await;

        handle.add_trusted_peer(peer_id, enr);
        events.next().await.unwrap();
    }
}
