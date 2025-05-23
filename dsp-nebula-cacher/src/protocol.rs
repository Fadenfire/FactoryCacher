use crate::nebula_protocol::NebulaPacket;
use bytes::{Buf, BufMut};
use hex_literal::hex;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct InitialConnectionInfo {
	pub client_uri: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct DedupedPacketDescription {
	pub packet: NebulaPacket,
	pub original_packet_size: u64,
}

#[derive(Debug)]
pub struct StartDedupIndicator {
	pub dedup_id: u64,
}

impl StartDedupIndicator {
	pub const MAGIC_NUMBER: &'static [u8] = &hex!("17f84ac897b3676995b8f5fbc3a10c9beda02aab0e3e752e8b46a1af40cf860f");
	
	pub fn decode(mut buf: &[u8]) -> Option<Self> {
		if !buf.starts_with(Self::MAGIC_NUMBER) {
			return None;
		}
		
		buf.advance(Self::MAGIC_NUMBER.len());
		
		Some(Self {
			dedup_id: buf.try_get_u64().ok()?,
		})
	}
	
	pub fn encode(&self, mut buf: impl BufMut) {
		buf.put(Self::MAGIC_NUMBER);
		buf.put_u64(self.dedup_id);
	}
}