use lightning::onion_message::packet::OnionMessageContents;
use lightning::util::ser::{Writeable, Writer};

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
