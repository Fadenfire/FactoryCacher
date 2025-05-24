use std::collections::HashMap;
use std::io::{Read, Write};
use anyhow::Context;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use common::dedup;
use common::dedup::ChunkKey;

const GAME_DATA_PACKET_ID: u64 = fnv1_hash(b"NebulaModel.Packets.Session.GlobalGameDataResponse");
const FACTORY_DATA_PACKET_ID: u64 = fnv1_hash(b"NebulaModel.Packets.Planet.FactoryData");

#[derive(Debug, Clone)]
pub struct GlobalGameDataHeader {
	pub data_type: u8,
	pub data_length: u32,
}

impl GlobalGameDataHeader {
	pub fn decode(mut buf: impl Buf) -> anyhow::Result<Self> {
		Ok(Self {
			data_type: buf.try_get_u8()?,
			data_length: buf.try_get_u32_le()?,
		})
	}
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GlobalGameDataPacket {
	pub data_type: u8,
	pub data_chunk_list: Vec<ChunkKey>,
}

impl GlobalGameDataPacket {
	pub fn deconstruct(
		header: GlobalGameDataHeader,
		mut packet_data: Bytes,
		all_chunks: &mut HashMap<ChunkKey, Bytes>
	) -> anyhow::Result<Self> {
		let data_chunk_list =
			deconstruct_lz4_data(&mut packet_data, header.data_length as usize, all_chunks)?;
		
		Ok(Self {
			data_type: header.data_type,
			data_chunk_list,
		})
	}
	
	pub fn reconstruct(self, chunks: &HashMap<ChunkKey, Bytes>) -> anyhow::Result<Bytes> {
		let mut output_data = BytesMut::new();
		
		let data = reconstruct_lz4_data(&self.data_chunk_list, chunks)?;
		
		output_data.put_u64_le(GAME_DATA_PACKET_ID);
		output_data.put_u8(self.data_type);
		output_data.put_u32_le(data.len().try_into()?);
		output_data.put(data);
		
		Ok(output_data.freeze())
	}
}

#[derive(Debug, Clone)]
pub struct FactoryDataHeader {
	pub planet_id: u32,
	pub data_length: u32,
}

impl FactoryDataHeader {
	pub fn decode(mut buf: impl Buf) -> anyhow::Result<Self> {
		Ok(Self {
			planet_id: buf.try_get_u32_le()?,
			data_length: buf.try_get_u32_le()?,
		})
	}
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FactoryDataPacket {
	pub planet_id: u32,
	pub data_chunk_list: Vec<ChunkKey>,
	pub terrain_data_chunk_list: Vec<ChunkKey>,
}

impl FactoryDataPacket {
	pub fn deconstruct(
		header: FactoryDataHeader,
		mut packet_data: Bytes,
		all_chunks: &mut HashMap<ChunkKey, Bytes>
	) -> anyhow::Result<Self> {
		let data_chunk_list =
			deconstruct_lz4_data(&mut packet_data, header.data_length as usize, all_chunks)?;
		
		let terrain_data_length = packet_data.try_get_u32_le()?;
		let terrain_data = try_split_to(&mut packet_data, terrain_data_length as usize)?;
		let terrain_data_chunk_list = dedup::chunk_data(&terrain_data, all_chunks);
		
		Ok(Self {
			planet_id: header.planet_id,
			data_chunk_list,
			terrain_data_chunk_list
		})
	}
	
	pub fn reconstruct(self, chunks: &HashMap<ChunkKey, Bytes>) -> anyhow::Result<Bytes> {
		let mut output_data = BytesMut::new();
		
		output_data.put_u64_le(FACTORY_DATA_PACKET_ID);
		output_data.put_u32_le(self.planet_id);
		
		let data = reconstruct_lz4_data(&self.data_chunk_list, chunks)?;
		
		output_data.put_u32_le(data.len().try_into()?);
		output_data.put(data);
		
		let mut terrain_data = Vec::new();
		
		for chunk_key in &self.terrain_data_chunk_list {
			let chunk = chunks.get(chunk_key).ok_or_else(|| anyhow::anyhow!("Chunk key doesn't exist"))?;
			
			terrain_data.extend_from_slice(chunk);
		}
		
		output_data.put_u32_le(terrain_data.len().try_into()?);
		output_data.extend_from_slice(&terrain_data);
		
		Ok(output_data.freeze())
	}
}

#[derive(Debug, Clone)]
pub enum NebulaPacketHeader {
	GlobalGameData(GlobalGameDataHeader),
	FactoryData(FactoryDataHeader),
}

impl NebulaPacketHeader {
	pub fn decode(mut buf: impl Buf) -> anyhow::Result<Option<Self>> {
		let packet_type = buf.try_get_u64_le()?;
		
		match packet_type {
			GAME_DATA_PACKET_ID => Ok(Some(Self::GlobalGameData(GlobalGameDataHeader::decode(buf)?))),
			FACTORY_DATA_PACKET_ID => Ok(Some(Self::FactoryData(FactoryDataHeader::decode(buf)?))),
			_ => Ok(None),
		}
	}
	
	pub fn approx_size(&self) -> usize {
		match self {
			Self::GlobalGameData(header) => header.data_length as usize,
			Self::FactoryData(header) => header.data_length as usize,
		}
	}
	
	pub fn deconstruct(self,
		packet_data: Bytes,
		all_chunks: &mut HashMap<ChunkKey, Bytes>
	) -> anyhow::Result<NebulaPacket> {
		match self {
			Self::GlobalGameData(header) =>
				Ok(NebulaPacket::GlobalGameData(GlobalGameDataPacket::deconstruct(header, packet_data, all_chunks)?)),
			Self::FactoryData(header) =>
				Ok(NebulaPacket::FactoryData(FactoryDataPacket::deconstruct(header, packet_data, all_chunks)?)),
		}
	}
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum NebulaPacket {
	GlobalGameData(GlobalGameDataPacket),
	FactoryData(FactoryDataPacket),
}

impl NebulaPacket {
	pub fn reconstruct(self, chunks: &HashMap<ChunkKey, Bytes>) -> anyhow::Result<Bytes> {
		match self {
			Self::GlobalGameData(packet) => packet.reconstruct(chunks),
			Self::FactoryData(packet) => packet.reconstruct(chunks),
		}
	}
	
	pub fn required_chunks(&self) -> Vec<ChunkKey> {
		match self {
			NebulaPacket::GlobalGameData(packet) => packet.data_chunk_list.clone(),
			NebulaPacket::FactoryData(packet) => packet.data_chunk_list.iter()
				.copied()
				.chain(packet.terrain_data_chunk_list.iter().copied())
				.collect(),
		}
	}
}

fn deconstruct_lz4_data(
	packet_data: &mut Bytes,
	data_length: usize,
	all_chunks: &mut HashMap<ChunkKey, Bytes>
) -> anyhow::Result<Vec<ChunkKey>> {
	let compressed_data = try_split_to(packet_data, data_length)?;
	let decompressed_data = decompress_lz4(&compressed_data).context("Decompressing lz4 data")?;
	
	let chunk_list = dedup::chunk_data(&decompressed_data, all_chunks);
	
	Ok(chunk_list)
}

fn reconstruct_lz4_data(chunk_list: &[ChunkKey], chunks: &HashMap<ChunkKey, Bytes>) -> anyhow::Result<Bytes> {
	let mut encoder = lz4_flex::frame::FrameEncoder::new(Vec::new());
	
	for chunk_key in chunk_list {
		let chunk = chunks.get(chunk_key).ok_or_else(|| anyhow::anyhow!("Chunk key doesn't exist"))?;
		
		encoder.write_all(&chunk)?;
	}
	
	Ok(encoder.finish()?.into())
}

fn try_split_to(packet_data: &mut Bytes, data_length: usize) -> anyhow::Result<Bytes> {
	if packet_data.remaining() < data_length {
		return Err(anyhow::anyhow!("Packet data is too short"));
	}
	
	Ok(packet_data.split_to(data_length))
}

fn decompress_lz4(input: &[u8]) -> anyhow::Result<Vec<u8>> {
	let mut decompressed = Vec::new();
	
	let mut decoder = lz4_flex::frame::FrameDecoder::new(input);
	decoder.read_to_end(&mut decompressed)?;
	
	Ok(decompressed)
}

const fn fnv1_hash(data: &[u8]) -> u64 {
	let mut hash = 14695981039346656037;
	let mut i = 0;
	
	while i < data.len() {
		hash ^= data[i] as u64;
		hash = hash.wrapping_mul(1099511628211);
		
		i += 1;
	}
	
	hash
}
