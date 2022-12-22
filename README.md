# `consentry`
## 🏗️🚧 WIP ️🏗🚧 

**Ethereum consensus network sentry**

## Overview
Consentry is a standalone consensus networking service, for listening to events like **blocks**, **attestations** and [more](https://github.com/ethereum/consensus-specs/blob/dev/specs/phase0/p2p-interface.md#global-topics) over the libp2p consensus network.

The repo is adapted from the [`lighthouse_network`](https://github.com/sigp/lighthouse/tree/stable/beacon_node/lighthouse_network) crate.

## Running
Currently, it just prints out peer connection events and received blocks:
```
RUST_LOG=info cargo run --release
```
Can also tune `RUST_LOG` to more granular tracing like `debug` or `trace`.