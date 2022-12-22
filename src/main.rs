use std::{str::FromStr, sync::Arc, time::Duration};

use consentry::{
    service::Network, types::GossipKind, Context, Enr, NetworkConfig, NetworkEvent, PubsubMessage,
};
use tracing::info;
use tracing_subscriber::{EnvFilter, FmtSubscriber};
use types::{ChainSpec, EthSpec, ForkContext, ForkName, Hash256, MainnetEthSpec, MinimalEthSpec};

#[tokio::main]
async fn main() {
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    // https://github.com/eth-clients/eth2-mainnet/blob/master/bootnodes.txt
    let boot_enrs = vec![
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
    ];

    let cfg = NetworkConfig {
        boot_nodes_enr: boot_enrs,
        libp2p_port: 9001,
        discovery_port: 9001,
        upnp_enabled: true,
        ..Default::default()
    };

    let fork = ForkName::Merge;

    // Populate the chain spec with mainnet parameters
    let mainnet_spec = ChainSpec::mainnet();

    // Get the merge slot
    let merge_slot = mainnet_spec
        .fork_epoch(fork)
        .unwrap()
        .start_slot(MainnetEthSpec::slots_per_epoch());

    // https://eth2book.info/bellatrix/part3/containers/state/#beacon-state
    let genesis_validators_root =
        Hash256::from_str("0x4b363db94e286120d76eb905340fdd4e54bfe9f06bf33ff6cf5ad27f511bfe95")
            .unwrap();

    // Build the merge fork context
    let merge_fork_context =
        ForkContext::new::<MainnetEthSpec>(merge_slot, genesis_validators_root, &mainnet_spec);

    // Build the network service context
    let ctx = Context {
        config: &cfg,
        enr_fork_id: mainnet_spec
            .enr_fork_id::<MainnetEthSpec>(merge_slot, genesis_validators_root),
        fork_context: Arc::new(merge_fork_context),
        chain_spec: &mainnet_spec,
        gossipsub_registry: None,
    };

    let (mut network, globals) = Network::<usize, MainnetEthSpec>::new(ctx).await.unwrap();

    // Subscribe to new blocks
    network.subscribe_kind(GossipKind::BeaconBlock);

    let handle = Arc::clone(&globals);
    tokio::task::spawn(async move {
        loop {
            info!(peer_count = handle.connected_peers(), "Stats");
            tokio::time::sleep(Duration::from_secs(5)).await
        }
    });

    loop {
        #[allow(clippy::collapsible_match)]
        match network.next_event().await {
            NetworkEvent::PeerConnectedIncoming(id) => {
                info!(peer = ?id, "Peer connected (incoming)");
            }
            NetworkEvent::PeerConnectedOutgoing(id) => {
                info!(peer = ?id, "Peer connected (outgoing)");
            }
            NetworkEvent::PeerDisconnected(id) => {
                info!(peer = ?id, "Peer disconnected");
            }
            NetworkEvent::PubsubMessage {
                source, message, ..
            } => {
                if let PubsubMessage::BeaconBlock(block) = message {
                    let slot = block.slot();
                    let msg = block.message();
                    let _header = msg.block_header();
                    let body = msg.body();
                    let _exec = body.execution_payload().unwrap();
                    info!(peer = ?source, %slot, "Received block");
                }
            }
            _ => {}
        }
    }
}

pub fn fork_context(fork_name: ForkName) -> ForkContext {
    let chain_spec = ChainSpec::mainnet();

    let current_slot = chain_spec
        .fork_epoch(fork_name)
        .unwrap()
        .start_slot(MinimalEthSpec::slots_per_epoch());

    ForkContext::new::<MinimalEthSpec>(current_slot, Hash256::zero(), &chain_spec)
}
