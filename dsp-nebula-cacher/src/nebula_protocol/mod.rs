mod global_game_data;
mod factory_data;
mod dyson_sphere_data;

use std::collections::HashMap;
use std::io::{Read, Write};
use anyhow::Context;
use bytes::{Buf, Bytes};
use futures_util::{pin_mut, StreamExt};
use serde::{Deserialize, Serialize};
use common::dedup::{ChunkList, ChunkProvider};
use common::dedup;
use common::dedup::ChunkKey;
use dyson_sphere_data::{DysonSphereDataHeader, DysonSphereDataPacket};
use factory_data::{FactoryDataHeader, FactoryDataPacket};
use global_game_data::{GlobalGameDataHeader, GlobalGameDataPacket};

const GAME_DATA_PACKET_ID: u64 = fnv1_hash(b"NebulaModel.Packets.Session.GlobalGameDataResponse");
const FACTORY_DATA_PACKET_ID: u64 = fnv1_hash(b"NebulaModel.Packets.Planet.FactoryData");
const DYSON_SPHERE_DATA_PACKET_ID: u64 = fnv1_hash(b"NebulaModel.Packets.Universe.DysonSphereData");

#[derive(Debug, Clone)]
pub enum NebulaPacketHeader {
	GlobalGameData(GlobalGameDataHeader),
	FactoryData(FactoryDataHeader),
	DysonSphereData(DysonSphereDataHeader),
}

impl NebulaPacketHeader {
	pub fn decode(mut buf: impl Buf) -> anyhow::Result<Option<Self>> {
		let packet_type = buf.try_get_u64_le()?;
		
		let result = match packet_type {
			GAME_DATA_PACKET_ID => Some(Self::GlobalGameData(GlobalGameDataHeader::decode(buf)?)),
			FACTORY_DATA_PACKET_ID => Some(Self::FactoryData(FactoryDataHeader::decode(buf)?)),
			DYSON_SPHERE_DATA_PACKET_ID => Some(Self::DysonSphereData(DysonSphereDataHeader::decode(buf)?)),
			_ => None,
		};
		
		Ok(result)
	}
	
	pub fn approx_size(&self) -> usize {
		match self {
			Self::GlobalGameData(header) => header.approx_size(),
			Self::FactoryData(header) => header.approx_size(),
			Self::DysonSphereData(header) => header.approx_size(),
		}
	}
	
	pub fn deconstruct(self,
		packet_data: Bytes,
		all_chunks: &mut HashMap<ChunkKey, Bytes>,
	) -> anyhow::Result<NebulaPacket> {
		let packet = match self {
			Self::GlobalGameData(header) =>
				NebulaPacket::GlobalGameData(GlobalGameDataPacket::deconstruct(header, packet_data, all_chunks)?),
			Self::FactoryData(header) =>
				NebulaPacket::FactoryData(FactoryDataPacket::deconstruct(header, packet_data, all_chunks)?),
			Self::DysonSphereData(header) =>
				NebulaPacket::DysonSphereData(DysonSphereDataPacket::deconstruct(header, packet_data, all_chunks)?),
		};
		
		Ok(packet)
	}
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum NebulaPacket {
	GlobalGameData(GlobalGameDataPacket),
	FactoryData(FactoryDataPacket),
	DysonSphereData(DysonSphereDataPacket),
}

impl NebulaPacket {
	pub async fn reconstruct(self, chunks: &mut impl ChunkProvider) -> anyhow::Result<Bytes> {
		match self {
			Self::GlobalGameData(packet) => packet.reconstruct(chunks).await,
			Self::FactoryData(packet) => packet.reconstruct(chunks).await,
			Self::DysonSphereData(packet) => packet.reconstruct(chunks).await,
		}
	}
}

fn deconstruct_lz4_data(
	packet_data: &mut Bytes,
	data_length: usize,
	all_chunks: &mut HashMap<ChunkKey, Bytes>
) -> anyhow::Result<ChunkList> {
	let compressed_data = try_split_to(packet_data, data_length)?;
	let decompressed_data = decompress_lz4(&compressed_data).context("Decompressing lz4 data")?;
	
	let chunk_list = dedup::chunk_data(&decompressed_data, all_chunks);
	
	Ok(chunk_list)
}

async fn reconstruct_lz4_data(chunk_list: &ChunkList, chunks: &mut impl ChunkProvider) -> anyhow::Result<Bytes> {
	let mut encoder = lz4_flex::frame::FrameEncoder::new(Vec::new());
	
	let reconstructed_chunks = dedup::reconstruct_data(&chunk_list, chunks);
	pin_mut!(reconstructed_chunks);
	
	while let Some(chunk) = reconstructed_chunks.next().await {
		encoder.write_all(&chunk?)?;
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
