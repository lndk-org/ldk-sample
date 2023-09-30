use lightning::io::Read;
use lightning::ln::msgs::DecodeError;
use lightning::onion_message::{OnionMessageContents, CustomOnionMessageHandler, PendingOnionMessage};
use lightning::util::ser::{Writeable, Writer};
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
}

impl CustomOnionMessageHandler for OnionMessageHandler {
	type CustomMessage = UserOnionMessageContents;

	fn handle_custom_message(&self, msg: Self::CustomMessage) -> Option<UserOnionMessageContents> {
		println!("Received a new custom message!");
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

        // We don't need this for our purposes yet.
        fn release_pending_custom_messages(&self) -> Vec<PendingOnionMessage<Self::CustomMessage>> {
		unimplemented!();
        }
}
