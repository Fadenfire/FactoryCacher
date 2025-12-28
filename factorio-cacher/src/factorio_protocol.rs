use crate::rev_crc::RevCRC;
use bitflags::bitflags;
use bytes::{Buf, BufMut, Bytes, BytesMut, TryGetError};
use common::utils::BufExt;
use crc::Crc;
use serde::{Deserialize, Serialize};

pub const FACTORIO_CRC: Crc<u32> = Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);
pub const FACTORIO_REV_CRC: RevCRC = RevCRC::new(&FACTORIO_CRC);

pub const TRANSFER_BLOCK_SIZE: u32 = 503;

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum PacketType {
	ServerToClientHeartbeat,
	TransferBlockRequest,
	TransferBlock,
	Unknown(u8),
}

impl From<u8> for PacketType {
	fn from(val: u8) -> Self {
		match val {
			7 => PacketType::ServerToClientHeartbeat,
			12 => PacketType::TransferBlockRequest,
			13 => PacketType::TransferBlock,
			val => PacketType::Unknown(val),
		}
	}
}

impl Into<u8> for PacketType {
	fn into(self) -> u8 {
		match self {
			PacketType::ServerToClientHeartbeat => 7,
			PacketType::TransferBlockRequest => 12,
			PacketType::TransferBlock => 13,
			PacketType::Unknown(val) => val
		}
	}
}

#[derive(Debug)]
pub struct FactorioPacketHeader {
	pub packet_type: PacketType,
	pub fragmentation: Option<PacketFragmentationInfo>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct PacketFragmentationInfo {
	pub is_last_fragment: bool,
	pub message_id: u16,
	pub fragment_id: Option<u16>,
}

impl FactorioPacketHeader {
	pub fn new_unfragmented(packet_type: PacketType) -> Self {
		Self {
			packet_type,
			fragmentation: None,
		}
	}
	
	pub fn decode(mut data: Bytes) -> Result<(Self, Bytes), TryGetError> {
		let flags = data.try_get_u8()?;
		
		let packet_type = PacketType::from(flags & 0b00011111);
		let is_fragmented = (flags & 0b01000000) != 0;
		let is_last_fragment = (flags & 0b10000000) != 0;
		
		let mut fragmentation = None;
		
		if is_fragmented || is_last_fragment {
			let message_id = data.try_get_u16_le()?;
			
			let fragment_id = if is_fragmented {
				Some(data.try_get_factorio_varint16()?)
			} else {
				None
			};
			
			if (message_id & 0x8000) != 0 {
				// Skip confirm array
				let len = data.try_get_u32_le()?;
				data.try_advance(len as usize)?;
			}
			
			fragmentation = Some(PacketFragmentationInfo {
				is_last_fragment,
				message_id: message_id & 0x7FFF,
				fragment_id,
			})
		}
		
		let packet = Self {
			packet_type,
			fragmentation,
		};
		
		Ok((packet, data))
	}
	
	pub fn encode(&self, buf: &mut BytesMut) {
		assert!(self.fragmentation.is_none(), "Encoding fragmented packets is unsupported");
		
		let flags: u8 = self.packet_type.into();
		buf.put_u8(flags);
	}
}

pub trait FactorioPacket {
	const PACKET_TYPE: PacketType;
	
	fn encode(&self, buf: &mut BytesMut);
	
	fn encode_full_packet(&self) -> Bytes {
		let mut buf = BytesMut::new();
		
		FactorioPacketHeader::new_unfragmented(Self::PACKET_TYPE).encode(&mut buf);
		self.encode(&mut buf);
		
		buf.freeze()
	}
}

pub struct TransferBlockRequestPacket {
	pub block_id: u32,
}

impl TransferBlockRequestPacket {
	pub fn decode(mut data: Bytes) -> Result<Self, TryGetError> {
		Ok(Self {
			block_id: data.try_get_u32_le()?,
		})
	}
}

impl FactorioPacket for TransferBlockRequestPacket {
	const PACKET_TYPE: PacketType = PacketType::TransferBlockRequest;
	
	fn encode(&self, buf: &mut BytesMut) {
		buf.put_u32_le(self.block_id);
	}
}

pub struct TransferBlockPacket {
	pub block_id: u32,
	pub data: Bytes,
}

impl TransferBlockPacket {
	pub fn decode(mut data: Bytes) -> Result<Self, TryGetError> {
		Ok(Self {
			block_id: data.try_get_u32_le()?,
			data,
		})
	}
}

impl FactorioPacket for TransferBlockPacket {
	const PACKET_TYPE: PacketType = PacketType::TransferBlock;
	
	fn encode(&self, buf: &mut BytesMut) {
		buf.put_u32_le(self.block_id);
		buf.extend_from_slice(&self.data);
	}
}

bitflags! {
	#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
	pub struct HeartbeatFlags: u8 {
		const None = 0x00;
		const HasRequestsForHeartbeat = 0x01;
		const HasTickClosures = 0x02;
		const SingleTickClosure = 0x04;
		const LoadTickOnly = 0x08;
		const HasSynchronizerActions = 0x10;
	}
}

pub struct ServerToClientHeartbeatPacket {
	pub flags: HeartbeatFlags,
	pub data: Bytes,
}

impl ServerToClientHeartbeatPacket {
	pub const MAP_READY_FOR_DOWNLOAD_ACTION_ID: u8 = 5;
	
	pub fn decode(mut data: Bytes) -> Result<Self, TryGetError> {
		let flags = HeartbeatFlags::from_bits_retain(data.try_get_u8()?);
		data.try_get_u32_le()?; // Seq number
		
		// println!("Got heartbeat, seq num: {}, flags: {:?}, flags byte: {}", q, flags, flags.bits());
		
		Ok(Self {
			flags,
			data,
		})
	}
	
	pub fn try_decode_map_ready(mut self) -> Result<Option<FactorioWorldMetadata>, TryGetError> {
		if self.flags == HeartbeatFlags::HasSynchronizerActions {
			let action_count = self.data.try_get_factorio_varint32()?;
			
			if action_count > 0 {
				let action_type = self.data.try_get_u8()?;
				
				if action_type == Self::MAP_READY_FOR_DOWNLOAD_ACTION_ID {
					return Ok(Some(FactorioWorldMetadata::decode(&mut self.data)?));
				}
			}
		}
		
		Ok(None)
	}
}

#[derive(Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
pub struct FactorioWorldMetadata {
	pub world_size: u32,
	pub no_idea1: u32,
	pub aux_size: u32,
	pub no_idea2: u32,
	pub world_crc: u32,
}

impl FactorioWorldMetadata {
	pub fn decode(mut data: impl Buf) -> Result<Self, TryGetError> {
		Ok(Self {
			world_size: data.try_get_u32_le()?,
			no_idea1: data.try_get_u32_le()?,
			aux_size: data.try_get_u32_le()?,
			no_idea2: data.try_get_u32_le()?,
			world_crc: data.try_get_u32_le()?,
		})
	}
	
	pub fn encode(&self, mut buf: impl BufMut) {
		buf.put_u32_le(self.world_size);
		buf.put_u32_le(self.no_idea1);
		buf.put_u32_le(self.aux_size);
		buf.put_u32_le(self.no_idea2);
		buf.put_u32_le(self.world_crc);
	}
}