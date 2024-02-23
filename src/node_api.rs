use crate::disk::{persist_channel_peer, FilesystemLogger};
use crate::onion::{OnionMessageHandler, UserOnionMessageContents};
use crate::{
	BitcoindClient, ChainMonitor, ChannelManager, NetworkGraph, OnionMessengerType,
	P2PGossipSyncType, PeerManagerType,
};

use bitcoin::secp256k1::{PublicKey, Secp256k1};
use bitcoin::Network;
use lightning::blinded_path::BlindedPath;
use lightning::ln::ChannelId;
use lightning::offers::offer::{Offer, OfferBuilder, Quantity};
use lightning::offers::parse::Bolt12SemanticError;
use lightning::onion_message::messenger::Destination;
use lightning::routing::router::DefaultRouter;
use lightning::routing::scoring::{ProbabilisticScorer, ProbabilisticScoringFeeParameters};
use lightning::sign::KeysManager;
use lightning::util::config::{ChannelHandshakeConfig, ChannelHandshakeLimits, UserConfig};
use lightning::util::errors::APIError;
use lightning_persister::fs_store::FilesystemStore;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};
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
	pub bitcoind_client: Arc<BitcoindClient>,
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
		connect_peer_if_necessary(pubkey, peer_addr, self.peer_manager.clone()).await
	}

	pub async fn open_channel(
		&self, peer_pubkey: PublicKey, peer_addr: SocketAddr, chan_amt_sat: u64, push_msat: u64,
		announce_channel: bool,
	) -> Result<ChannelId, APIError> {
		if self.connect_to_peer(peer_pubkey, peer_addr).await.is_err() {
			return Err(APIError::ChannelUnavailable { err: "Cannot connect to peer".to_string() });
		};

		let peer_pubkey_and_ip_addr = peer_pubkey.to_string() + "@" + &peer_addr.to_string();
		match open_channel(
			peer_pubkey,
			chan_amt_sat,
			push_msat,
			announce_channel,
			false, // Without anchors for simplicity.
			self.channel_manager.clone(),
		) {
			Ok(channel_id) => {
				let peer_data_path = format!("{:?}/channel_peer_data", self.ldk_data_dir);
				let _ = persist_channel_peer(Path::new(&peer_data_path), &peer_pubkey_and_ip_addr);
				Ok(channel_id)
			},
			Err(e) => Err(e),
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
		match self.onion_messenger.send_onion_message(
			UserOnionMessageContents { tlv_type, data },
			destination,
			None,
		) {
			Ok(success) => {
				println!("SUCCESS: forwarded onion message to first hop {:?}", success);
				Ok(())
			},
			Err(e) => {
				println!("ERROR: failed to send onion message: {:?}", e);
				Ok(())
			},
		}
	}

	// Build an offer for receiving payments at this node. path_pubkeys lists the nodes the path will contain,
	// starting with the introduction node and ending in the destination node (the current node).
	pub async fn create_offer(
		&self, path_pubkeys: &[PublicKey], network: Network, msats: u64, quantity: Quantity,
		expiration: SystemTime,
	) -> Result<Offer, Bolt12SemanticError> {
		let secp_ctx = Secp256k1::new();
		let path =
			BlindedPath::new_for_message(path_pubkeys, &*self.keys_manager, &secp_ctx).unwrap();
		let (pubkey, _) = self.get_node_info();

		OfferBuilder::new("testing offer".to_string(), pubkey)
			.amount_msats(msats)
			.chain(network)
			.supported_quantity(quantity)
			.absolute_expiry(expiration.duration_since(SystemTime::UNIX_EPOCH).unwrap())
			.issuer("Foo Bar".to_string())
			.path(path)
			.build()
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

pub(crate) async fn connect_peer_if_necessary(
	pubkey: PublicKey, peer_addr: SocketAddr, peer_manager: Arc<PeerManagerType>,
) -> Result<(), ()> {
	for (node_pubkey, _) in peer_manager.get_peer_node_ids() {
		if node_pubkey == pubkey {
			return Ok(());
		}
	}
	let res = do_connect_peer(pubkey, peer_addr, peer_manager).await;
	if res.is_err() {
		println!("ERROR: failed to connect to peer");
	}
	res
}

pub(crate) async fn do_connect_peer(
	pubkey: PublicKey, peer_addr: SocketAddr, peer_manager: Arc<PeerManagerType>,
) -> Result<(), ()> {
	match lightning_net_tokio::connect_outbound(Arc::clone(&peer_manager), pubkey, peer_addr).await
	{
		Some(connection_closed_future) => {
			let mut connection_closed_future = Box::pin(connection_closed_future);
			loop {
				tokio::select! {
					_ = &mut connection_closed_future => return Err(()),
					_ = tokio::time::sleep(Duration::from_millis(10)) => {},
				};
				if peer_manager.get_peer_node_ids().iter().find(|(id, _)| *id == pubkey).is_some() {
					return Ok(());
				}
			}
		},
		None => Err(()),
	}
}

fn open_channel(
	peer_pubkey: PublicKey, channel_amt_sat: u64, push_msat: u64, announced_channel: bool,
	with_anchors: bool, channel_manager: Arc<ChannelManager>,
) -> Result<ChannelId, APIError> {
	let config = UserConfig {
		channel_handshake_limits: ChannelHandshakeLimits { ..Default::default() },
		channel_handshake_config: ChannelHandshakeConfig {
			announced_channel,
			negotiate_anchors_zero_fee_htlc_tx: with_anchors,
			..Default::default()
		},
		..Default::default()
	};

	channel_manager.create_channel(peer_pubkey, channel_amt_sat, push_msat, 0, None, Some(config))
}

pub(crate) async fn connect_peer_if_necessary(
	pubkey: PublicKey, peer_addr: SocketAddr, peer_manager: Arc<PeerManagerType>,
) -> Result<(), ()> {
	for (node_pubkey, _) in peer_manager.get_peer_node_ids() {
		if node_pubkey == pubkey {
			return Ok(());
		}
	}
	let res = do_connect_peer(pubkey, peer_addr, peer_manager).await;
	if res.is_err() {
		println!("ERROR: failed to connect to peer");
	}
	res
}

pub(crate) async fn do_connect_peer(
	pubkey: PublicKey, peer_addr: SocketAddr, peer_manager: Arc<PeerManagerType>,
) -> Result<(), ()> {
	match lightning_net_tokio::connect_outbound(Arc::clone(&peer_manager), pubkey, peer_addr).await
	{
		Some(connection_closed_future) => {
			let mut connection_closed_future = Box::pin(connection_closed_future);
			loop {
				tokio::select! {
					_ = &mut connection_closed_future => return Err(()),
					_ = tokio::time::sleep(Duration::from_millis(10)) => {},
				};
				if peer_manager.get_peer_node_ids().iter().find(|(id, _)| *id == pubkey).is_some() {
					return Ok(());
				}
			}
		},
		None => Err(()),
	}
}
