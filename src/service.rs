use std::collections::VecDeque;
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
pub struct ServiceConfig {
    pub boot_enrs: Vec<Enr>,
    pub libp2p_port: u16,
    pub discovery_port: u16,
    pub max_peers: usize,
    pub metrics_enabled: bool,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            boot_enrs: bootnode_enrs(),
            libp2p_port: 9000,
            discovery_port: 9000,
            max_peers: 50,
            metrics_enabled: true,
        }
    }
}

#[derive(Debug)]
pub enum ServiceCommand {
    Subscribe(GossipKind),
    PeerCount(oneshot::Sender<usize>),
}

#[derive(Clone, Debug)]
pub struct ServiceHandle {
    cmd_tx: UnboundedSender<ServiceCommand>,
}

impl ServiceHandle {
    pub fn subscribe_topic(&self, gossip_kind: GossipKind) {
        let _ = self.cmd_tx.send(ServiceCommand::Subscribe(gossip_kind));
    }

    pub async fn peer_count(&self) -> usize {
        let (tx, rx) = oneshot::channel();
        let _ = self.cmd_tx.send(ServiceCommand::PeerCount(tx));
        rx.await.unwrap_or(0)
    }
}

#[derive(Debug)]
pub struct Service {
    cfg: ServiceConfig,
    cmd_rx: UnboundedReceiver<ServiceCommand>,
    events_tx: Option<UnboundedSender<SentryMessage>>,
    handle: ServiceHandle,
}

impl Service {
    pub fn new(cfg: ServiceConfig) -> Service {
        let (tx, rx) = mpsc::unbounded_channel();
        Service {
            cfg,
            cmd_rx: rx,
            handle: ServiceHandle { cmd_tx: tx },
            events_tx: None,
        }
    }

    /// Returns a clone of the `ServiceHandle` which can be used to send commands to the service.
    pub fn handle(&self) -> ServiceHandle {
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
            network_load: 5,
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

        loop {
            tokio::select! {
                cmd = self.cmd_rx.recv() => {
                    if let Some(cmd) = cmd {
                        match cmd {
                            ServiceCommand::Subscribe(kind) => {
                                debug!(?kind, "New topic subscription");
                                network.subscribe_kind(kind);
                            }
                            ServiceCommand::PeerCount(tx) => {
                                let _ = tx.send(globals.connected_peers());
                            }
                        }
                    }
                }
                event = network.next_event() => {
                    match event {
                        NetworkEvent::PeerConnectedIncoming(id) => {
                            debug!(peer = ?id, "Peer connected (incoming)");
                        }
                        // NOTE: we have to send status messages when connecting here:
                        // https://github.com/ethereum/consensus-specs/blob/dev/specs/phase0/p2p-interface.md#status
                        NetworkEvent::PeerConnectedOutgoing(id) => {
                            let client_type = globals.client(&id);
                            debug!(peer = ?id, ?client_type, "Peer connected (outgoing)");
                            network.send_request(id, 10, Request::Status(highest_status.clone()));
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
