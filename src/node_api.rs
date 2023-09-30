use crate::disk::FilesystemLogger;
use crate::onion::{OnionMessageHandler, UserOnionMessageContents};
use crate::{
	BitcoindClient, ChainMonitor, ChannelManager, NetworkGraph, OnionMessengerType, PeerManagerType, P2PGossipSyncType,
};

use bitcoin::secp256k1::PublicKey;
use lightning::onion_message::{Destination, OnionMessagePath};
use lightning::routing::router::DefaultRouter;
use lightning::routing::scoring::{ProbabilisticScorer, ProbabilisticScoringFeeParameters};
use lightning::sign::KeysManager;
use lightning_persister::fs_store::FilesystemStore;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::watch::Sender;

pub(crate) type Router = DefaultRouter<
	Arc<NetworkGraph>,
	Arc<FilesystemLogger>,
	Arc<RwLock<Scorer>>,
	ProbabilisticScoringFeeParameters,
	Scorer,
>;
pub(crate) type Scorer = ProbabilisticScorer<Arc<NetworkGraph>, Arc<FilesystemLogger>>;

pub struct Node {
	pub(crate) logger: Arc<FilesystemLogger>,
	pub(crate) bitcoind_client: Arc<BitcoindClient>,
	pub(crate) persister: Arc<FilesystemStore>,
	pub(crate) chain_monitor: Arc<ChainMonitor>,
	pub(crate) keys_manager: Arc<KeysManager>,
	pub(crate) network_graph: Arc<NetworkGraph>,
	pub(crate) router: Arc<Router>,
	pub(crate) scorer: Arc<RwLock<Scorer>>,
	pub(crate) channel_manager: Arc<ChannelManager>,
	pub(crate) gossip_sync: Arc<P2PGossipSyncType>,
	pub(crate) onion_messenger: Arc<OnionMessengerType>,
	pub onion_message_handler: Arc<OnionMessageHandler>,
	pub(crate) peer_manager: Arc<PeerManagerType>,
	pub(crate) bp_exit: Sender<()>,
	pub(crate) background_processor: tokio::task::JoinHandle<Result<(), std::io::Error>>,
	pub(crate) stop_listen_connect: Arc<AtomicBool>,

	// Config values
	pub(crate) listening_port: u16,
	pub(crate) ldk_data_dir: TempDir,
}

impl Node {
	// get_node_info retrieves node_id and listening address.
	pub fn get_node_info(&self) -> (PublicKey, SocketAddr) {
		let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), self.listening_port);
		(self.channel_manager.get_our_node_id(), socket)
	}

	pub async fn connect_to_peer(
		&self, pubkey: PublicKey, peer_addr: SocketAddr,
	) -> Result<(), ()> {
		// If we're already connected to peer, then we're good to go.
		for (node_pubkey, _) in self.peer_manager.get_peer_node_ids() {
			if node_pubkey == pubkey {
				return Ok(());
			}
		}
		let res = match self.do_connect_peer(pubkey, peer_addr).await {
			Ok(_) => {
				println!("SUCCESS: connected to peer {}", pubkey);
				Ok(())
			}
			Err(e) => {
				println!("ERROR: failed to connect to peer: {e:?}");
				Err(())
			}
		};
		res
	}

	pub async fn do_connect_peer(
		&self, pubkey: PublicKey, peer_addr: SocketAddr,
	) -> Result<(), ()> {
		match lightning_net_tokio::connect_outbound(
			Arc::clone(&self.peer_manager),
			pubkey,
			peer_addr,
		)
		.await
		{
			Some(connection_closed_future) => {
				let mut connection_closed_future = Box::pin(connection_closed_future);
				loop {
					tokio::select! {
						_ = &mut connection_closed_future => return Err(()),
						_ = tokio::time::sleep(Duration::from_millis(10)) => {},
					};
					if self
						.peer_manager
						.get_peer_node_ids()
						.iter()
						.find(|(id, _)| *id == pubkey)
						.is_some()
					{
						return Ok(());
					}
				}
			}
			None => Err(()),
		}
	}

	pub async fn send_onion_message(
		&self, mut intermediate_nodes: Vec<PublicKey>, tlv_type: u64, data: Vec<u8>,
	) -> Result<(), ()> {
		if intermediate_nodes.len() == 0 {
			println!("Need to provide pubkey to send onion message");
			return Err(());
		}
		if tlv_type <= 64 {
			println!("Need an integral message type above 64");
			return Err(());
		}
		let destination = Destination::Node(intermediate_nodes.pop().unwrap());
		let message_path = OnionMessagePath { intermediate_nodes, destination };
		match self.onion_messenger.send_onion_message(
			message_path,
			UserOnionMessageContents { tlv_type, data }, // <dyn OnionMessageContents>::Custom()
			None,
		) {
			Ok(()) => {
				println!("SUCCESS: forwarded onion message to first hop");
				Ok(())
			}
			Err(e) => {
				println!("ERROR: failed to send onion message: {:?}", e);
				Ok(())
			}
		}
	}

	pub async fn stop(self) {
		// Disconnect our peers and stop accepting new connections. This ensures we don't continue
		// updating our channel data after we've stopped the background processor.
		self.stop_listen_connect.store(true, Ordering::Release);
		self.peer_manager.disconnect_all_peers();

		// Stop the background processor.
		if !self.bp_exit.is_closed() {
			self.bp_exit.send(()).unwrap();
			self.background_processor.await.unwrap().unwrap();
		}
	}
}
