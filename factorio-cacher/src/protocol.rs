use crate::factorio_protocol::FactorioWorldMetadata;
use crate::factorio_world::FactorioWorldDescription;
use bytes::{BufMut, Bytes, BytesMut};
use quinn_proto::coding::Codec;
use quinn_proto::VarInt;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const UDP_PEER_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Eq, PartialEq)]
pub struct Datagram {
	pub peer_id: VarInt,
	pub data: Bytes,
}

impl Datagram {
	pub fn new(peer_id: VarInt, data: Bytes) -> Self {
		Self {
			peer_id,
			data,
		}
	}
	
	pub fn decode(mut data: Bytes) -> anyhow::Result<Self> {
		let peer_id = VarInt::decode(&mut data)?;
		
		Ok(Self {
			peer_id,
			data,
		})
	}
	
	pub fn encode(&self, buffer: &mut BytesMut) {
		self.peer_id.encode(buffer);
		buffer.put_slice(&self.data);
	}
}

#[derive(Deserialize, Serialize)]
pub struct WorldReadyMessage {
	pub world: FactorioWorldDescription,
	pub old_info: FactorioWorldMetadata,
	pub new_info: FactorioWorldMetadata,
}
