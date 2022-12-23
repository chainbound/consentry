use consentry::{GossipKind, PubsubMessage, Service, ServiceConfig};
use futures::StreamExt;
use tracing::info;
use tracing_subscriber::{EnvFilter, FmtSubscriber};

#[tokio::main]
async fn main() {
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let mut svc = Service::new(ServiceConfig::default());

    let handle = svc.handle();

    handle.subscribe_topic(GossipKind::BeaconBlock);
    let mut events = svc.pubsub_event_stream();
    tokio::task::spawn(svc.start());

    while let Some(event) = events.next().await {
        if let PubsubMessage::BeaconBlock(block) = event {
            info!(slot = %block.slot(), hash = ?block.canonical_root(), "Received block");
        }

        info!("Peer count: {}", handle.peer_count().await);
    }
}
