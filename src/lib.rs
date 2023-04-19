/// This crate contains the main link for lighthouse to rust-libp2p. It therefore re-exports
/// all required libp2p functionality.
///
/// This crate builds and manages the libp2p services required by the beacon node.
#[macro_use]
extern crate lazy_static;
extern crate types as consensus_types;

mod config;
pub mod network;

#[allow(clippy::mutable_key_type)] // PeerId in hashmaps are no longer permitted by clippy
mod discovery;
mod internal_metrics;
mod peer_manager;
mod rpc;
mod service;
mod types;

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use std::str::FromStr;

/// Wrapper over a libp2p `PeerId` which implements `Serialize` and `Deserialize`
#[derive(Clone, Debug)]
pub struct PeerIdSerialized(libp2p::PeerId);

impl From<PeerIdSerialized> for PeerId {
    fn from(peer_id: PeerIdSerialized) -> Self {
        peer_id.0
    }
}

impl FromStr for PeerIdSerialized {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(
            PeerId::from_str(s).map_err(|e| format!("Invalid peer id: {}", e))?,
        ))
    }
}

impl Serialize for PeerIdSerialized {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for PeerIdSerialized {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s: String = Deserialize::deserialize(deserializer)?;
        Ok(Self(PeerId::from_str(&s).map_err(|e| {
            de::Error::custom(format!("Failed to deserialise peer id: {:?}", e))
        })?))
    }
}

use crate::types::{error, Enr, EnrSyncCommitteeBitfield, NetworkGlobals, Subnet, SubnetDiscovery};
use config::Config as NetworkConfig;
use discovery::{CombinedKeyExt, EnrExt, Eth2Enr};
use libp2p::gossipsub::TopicHash;
use libp2p::PeerId;
use libp2p::{multiaddr, Multiaddr};
use network::api_types::{Request, Response};
use network::utils::*;
use network::{Gossipsub, Network, NetworkEvent};
use peer_manager::peerdb::client::Client;

pub use crate::types::{GossipKind, GossipTopic, PubsubMessage};
pub use consensus_types::{
    BeaconBlock, BitVector, EthSpec, ExecutionPayload, FullPayload, MainnetEthSpec,
    SignedBeaconBlock,
};
pub use service::{Service, ServiceConfig, ServiceHandle};
