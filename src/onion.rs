use crate::disk::FilesystemLogger;
use crate::ChannelManager;
use bitcoin::secp256k1::{KeyPair, PublicKey, Secp256k1};
use lightning::blinded_path::payment::PaymentContext;
use lightning::blinded_path::payment::{
	Bolt12OfferContext, ForwardNode, ForwardTlvs, PaymentConstraints, PaymentRelay, ReceiveTlvs,
};
use lightning::blinded_path::BlindedPath;
use lightning::offers::invoice_request::InvoiceRequestFields;

use lightning::io::Read;
use lightning::ln::channelmanager::MIN_FINAL_CLTV_EXPIRY_DELTA;
use lightning::ln::features::BlindedHopFeatures;
use lightning::ln::msgs::DecodeError;
use lightning::offers::invoice::UnsignedBolt12Invoice;
use lightning::offers::offer::OfferId;
use lightning::onion_message::messenger::{CustomOnionMessageHandler, PendingOnionMessage};
use lightning::onion_message::offers::{OffersMessage, OffersMessageHandler};
use lightning::onion_message::packet::OnionMessageContents;
use lightning::sign::{EntropySource, KeysManager};
use lightning::util::logger::Logger;
use lightning::util::ser::{Writeable, Writer};
use lightning::util::string::UntrustedString;
use lightning::{log_error, log_info};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct UserOnionMessageContents {
	pub tlv_type: u64,
	pub data: Vec<u8>,
}

impl OnionMessageContents for UserOnionMessageContents {
	fn tlv_type(&self) -> u64 {
		self.tlv_type
	}
}

impl Writeable for UserOnionMessageContents {
	fn write<W: Writer>(&self, w: &mut W) -> Result<(), std::io::Error> {
		w.write_all(&self.data)
	}
}

// An extremely basic message handler needed for our integration tests.
#[derive(Clone)]
pub struct OnionMessageHandler {
	pub messages: Arc<Mutex<VecDeque<UserOnionMessageContents>>>,
	pub(crate) logger: Arc<FilesystemLogger>,
	pub(crate) keys_manager: Arc<KeysManager>,
	pub(crate) channel_manager: Arc<ChannelManager>,
	pub(crate) node_id: PublicKey,
}

impl CustomOnionMessageHandler for OnionMessageHandler {
	type CustomMessage = UserOnionMessageContents;

	fn handle_custom_message(&self, msg: Self::CustomMessage) -> Option<UserOnionMessageContents> {
		log_info!(self.logger, "Received a new custom message!");
		self.messages.lock().unwrap().push_back(msg.clone());
		Some(msg)
	}

	fn read_custom_message<R: Read>(
		&self, message_type: u64, buffer: &mut R,
	) -> Result<Option<Self::CustomMessage>, DecodeError> {
		let mut buf = vec![];
		let _ = buffer.read_to_end(&mut buf);
		Ok(Some(UserOnionMessageContents { tlv_type: message_type, data: buf.to_vec() }))
	}

	fn release_pending_custom_messages(&self) -> Vec<PendingOnionMessage<Self::CustomMessage>> {
		vec![]
	}
}

impl OffersMessageHandler for OnionMessageHandler {
	fn handle_message(&self, message: OffersMessage) -> Option<OffersMessage> {
		log_info!(self.logger, "Received a new offers message!");
		match message {
			OffersMessage::InvoiceRequest(ref invoice_request) => {
				let (payment_hash, payment_secret) =
					self.channel_manager.create_inbound_payment(None, 3600, None).unwrap();

				// Reminder that to keep things simple for our tests, we assume we're only connected to zero or one channel
				// for now.
				let chans = self.channel_manager.list_channels();
				let htlc_minimum_msat =
					if chans.len() == 0 { 1 } else { chans[0].inbound_htlc_minimum_msat.unwrap() };

				let payee_tlvs = ReceiveTlvs {
					payment_secret,
					payment_constraints: PaymentConstraints {
						max_cltv_expiry: u32::max_value(),
						htlc_minimum_msat,
					},
					payment_context: PaymentContext::Bolt12Offer(Bolt12OfferContext {
						offer_id: OfferId(self.keys_manager.get_secure_random_bytes()),
						invoice_request: InvoiceRequestFields {
							payer_id: invoice_request.payer_id(),
							quantity: invoice_request.quantity(),
							payer_note_truncated: invoice_request
								.payer_note()
								.map(|note| UntrustedString(note.to_string())),
						},
					}),
				};

				let forwarding_tlv = ForwardTlvs {
					short_channel_id: chans[0].short_channel_id.unwrap(),
					payment_relay: PaymentRelay {
						cltv_expiry_delta: chans[0].config.unwrap().cltv_expiry_delta,
						fee_proportional_millionths: chans[0]
							.config
							.unwrap()
							.forwarding_fee_proportional_millionths,
						fee_base_msat: chans[0].config.unwrap().forwarding_fee_base_msat,
					},
					payment_constraints: PaymentConstraints {
						max_cltv_expiry: 1000000,
						htlc_minimum_msat: 1,
					},
					features: BlindedHopFeatures::empty(),
				};
				let intermediate_nodes = &[ForwardNode {
					tlvs: forwarding_tlv,
					node_id: chans[0].counterparty.node_id,
					htlc_maximum_msat: chans[0].inbound_htlc_maximum_msat.unwrap(),
				}];

				let secp_ctx = Secp256k1::new();
				let blinded_path = BlindedPath::new_for_payment(
					intermediate_nodes,
					self.node_id,
					payee_tlvs,
					chans[0].inbound_htlc_maximum_msat.unwrap(),
					MIN_FINAL_CLTV_EXPIRY_DELTA,
					&*self.keys_manager,
					&secp_ctx,
				)
				.unwrap();

				let secret_key = self.keys_manager.get_node_secret_key();
				let keys = KeyPair::from_secret_key(&secp_ctx, &secret_key);
				let pubkey = PublicKey::from(keys);
				let wpubkey_hash = bitcoin::PublicKey::new(pubkey).wpubkey_hash().unwrap();

				return Some(OffersMessage::Invoice(
					invoice_request
						.respond_with(vec![blinded_path], payment_hash)
						.unwrap()
						.relative_expiry(3600)
						.fallback_v0_p2wpkh(&wpubkey_hash)
						.build()
						.unwrap()
						.sign(|message: &UnsignedBolt12Invoice| {
							Ok(secp_ctx
								.sign_schnorr_no_aux_rand(message.as_ref().as_digest(), &keys))
						})
						.expect("failed verifying signature"),
				));
			},
			_ => {
				log_error!(self.logger, "Unsupported offers message type");
				return None;
			},
		};
	}

	fn release_pending_messages(&self) -> Vec<PendingOnionMessage<OffersMessage>> {
		vec![]
	}
}
