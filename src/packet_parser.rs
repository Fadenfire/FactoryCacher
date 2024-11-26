#![allow(dead_code)]

use std::io::Cursor;
use bitflags::bitflags;
use crate::io_utils::{BufExt, UnexpectedEOF};
use bytes::{Buf, BufMut, Bytes, BytesMut};

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

pub struct FactorioPacket {
	pub packet_type: PacketType,
	pub is_fragmented: bool,
	pub is_last_fragment: bool,
	pub data: Bytes,
}

impl FactorioPacket {
	pub fn new(packet_type: PacketType, data: Bytes) -> Self {
		Self {
			packet_type,
			is_fragmented: false,
			is_last_fragment: false,
			data,
		}
	}
	
	pub fn encode(&self) -> Bytes {
		let mut buf = BytesMut::new();
		
		let mut flags: u8 = self.packet_type.into();
		if self.is_fragmented { flags |= 0b01000000; }
		if self.is_last_fragment { flags |= 0b10000000; }
		
		buf.put_u8(flags);
		buf.extend_from_slice(&self.data);
		
		buf.freeze()
	}
}

pub fn parse_packet(mut data: Bytes) -> Result<FactorioPacket, UnexpectedEOF> {
	let flags = data.try_get_u8()?;
	
	let packet = FactorioPacket {
		packet_type: PacketType::from(flags & 0b00011111),
		is_fragmented: (flags & 0b01000000) != 0,
		is_last_fragment: (flags & 0b10000000) != 0,
		data
	};
	
	Ok(packet)
}

pub struct TransferBlockRequestPacket {
	pub block_id: u32,
}

impl TransferBlockRequestPacket {
	pub fn decode(mut data: Bytes) -> Result<Self, UnexpectedEOF> {
		Ok(Self {
			block_id: data.try_get_u32_le()?,
		})
	}
	
	pub fn encode(&self, buf: &mut BytesMut) {
		buf.put_u32_le(self.block_id);
	}
	
	pub fn as_factorio_packet(&self) -> FactorioPacket {
		let mut buf = BytesMut::new();
		self.encode(&mut buf);
		
		FactorioPacket::new(PacketType::TransferBlockRequest, buf.freeze())
	}
}

pub struct TransferBlockPacket {
	pub block_id: u32,
	pub data: Bytes,
}

impl TransferBlockPacket {
	pub fn decode(mut data: Bytes) -> Result<Self, UnexpectedEOF> {
		Ok(Self {
			block_id: data.try_get_u32_le()?,
			data,
		})
	}
	
	pub fn encode(&self, buf: &mut BytesMut) {
		buf.put_u32_le(self.block_id);
		buf.extend_from_slice(&self.data);
	}
	
	pub fn as_factorio_packet(&self) -> FactorioPacket {
		let mut buf = BytesMut::new();
		self.encode(&mut buf);
		
		FactorioPacket::new(PacketType::TransferBlock, buf.freeze())
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
	pub content: Bytes,
	
	pub flags: HeartbeatFlags,
	pub seq_number: u32,
	pub map_ready_to_download_data: Option<(MapReadyForDownloadData, usize)>,
}

impl ServerToClientHeartbeatPacket {
	const MAP_READY_FOR_DOWNLOAD_ACTION_ID: u8 = 5;
	
	pub fn decode(data: Bytes) -> Result<Self, UnexpectedEOF> {
		let mut cursor = Cursor::new(&data);
		
		let flags = HeartbeatFlags::from_bits_retain(cursor.try_get_u8()?);
		let seq_number = cursor.try_get_u32_le()?;
		
		let mut map_ready_to_download_data = None;
		
		if flags == HeartbeatFlags::HasSynchronizerActions {
			let action_count = cursor.try_get_factorio_varint32()?;
			
			if action_count > 0 {
				let action_type = cursor.try_get_u8()?;
				
				if action_type == Self::MAP_READY_FOR_DOWNLOAD_ACTION_ID {
					let pos = cursor.position() as usize;
					let action = MapReadyForDownloadData::decode(&mut cursor)?;
					
					map_ready_to_download_data = Some((action, pos));
				}
			}
		}
		
		Ok(Self {
			flags,
			seq_number,
			content: data,
			
			map_ready_to_download_data,
		})
	}
	
	pub fn encode(&self, buf: &mut BytesMut) {
		let initial_buf_size = buf.len();
		buf.extend_from_slice(&self.content);
		
		if let Some((action, pos)) = self.map_ready_to_download_data.as_ref() {
			action.encode(&mut buf[initial_buf_size + pos..]);
		}
	}
	
	pub fn as_factorio_packet(&self) -> FactorioPacket {
		let mut buf = BytesMut::new();
		self.encode(&mut buf);
		
		FactorioPacket::new(PacketType::ServerToClientHeartbeat, buf.freeze())
	}
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct MapReadyForDownloadData {
	pub world_size: u32,
	pub no_idea1: u32,
	pub aux_size: u32,
	pub no_idea2: u32,
	pub world_crc: u32,
}

impl MapReadyForDownloadData {
	pub fn decode(mut data: impl Buf) -> Result<Self, UnexpectedEOF> {
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