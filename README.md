# `consentry`
## 🏗️🚧 WIP ️🏗🚧 

**Ethereum consensus network sentry**

## Overview
Consentry is a standalone consensus networking service, for listening to events like **blocks**, **attestations** and [more](https://github.com/ethereum/consensus-specs/blob/dev/specs/phase0/p2p-interface.md#global-topics) over the libp2p consensus network.

The repo is adapted from the [`lighthouse_network`](https://github.com/sigp/lighthouse/tree/stable/beacon_node/lighthouse_network) crate.

## Usage
This is more or less the [main example](./examples/main.rs):
```rs
use consentry::{GossipKind, Service, ServiceConfig};
use futures::StreamExt;

#[tokio::main]
async fn main() {
    // Build the service. ServiceConfig::default() will
    // come with sensible defaults (and bootnodes)
    let mut svc = Service::new(ServiceConfig::default());

    // Get a handle for interacting with the service
    let handle = svc.handle();

    // Subscribe to any `GossipKind` topics
    handle.subscribe_topic(GossipKind::BeaconBlock);

    // Open the event stream on which these topics will be sent
    let mut events = svc.pubsub_event_stream();

    // Spawn the service
    tokio::task::spawn(svc.start());

    while let Some(event) = events.next().await {
        println!("Event: {:?}", event.kind());
        println!("Peer count: {}", handle.peer_count().await);
    }
}
```

## Running Example
Run the example with:
```
RUST_LOG=consentry=info cargo run --example main
```
This will print out received blocks and peer counts.
You can also tune `RUST_LOG` to more granular tracing like `debug` or `trace`.