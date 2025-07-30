use bytes::{Buf, BufMut, Bytes, BytesMut};
use std::collections::HashMap;
use common::chunks::ChunkProvider;
use common::dedup;
use common::dedup::ChunkKey;
use serde::{Deserialize, Serialize};
use crate::nebula_protocol;
use crate::nebula_protocol::FACTORY_DATA_PACKET_ID;

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
	
	pub fn approx_size(&self) -> usize {
		self.data_length as usize
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
			nebula_protocol::deconstruct_lz4_data(&mut packet_data, header.data_length as usize, all_chunks)?;
		
		let terrain_data_length = packet_data.try_get_u32_le()?;
		let terrain_data = nebula_protocol::try_split_to(&mut packet_data, terrain_data_length as usize)?;
		let terrain_data_chunk_list = dedup::chunk_data(&terrain_data, all_chunks);
		
		Ok(Self {
			planet_id: header.planet_id,
			data_chunk_list,
			terrain_data_chunk_list
		})
	}
	
	pub fn required_chunks(&self) -> Vec<ChunkKey> {
		self.data_chunk_list.iter()
			.copied()
			.chain(self.terrain_data_chunk_list.iter().copied())
			.collect()
	}
	
	pub async fn reconstruct(self, chunks: &mut impl ChunkProvider) -> anyhow::Result<Bytes> {
		let mut output_data = BytesMut::new();
		
		output_data.put_u64_le(FACTORY_DATA_PACKET_ID);
		output_data.put_u32_le(self.planet_id);
		
		let data = nebula_protocol::reconstruct_lz4_data(&self.data_chunk_list, chunks).await?;
		
		output_data.put_u32_le(data.len().try_into()?);
		output_data.put(data);
		
		let mut terrain_data = Vec::new();
		
		for &chunk_key in &self.terrain_data_chunk_list {
			let chunk = chunks.get_chunk(chunk_key).await?
				.ok_or_else(|| anyhow::anyhow!("Chunk key doesn't exist"))?;
			
			terrain_data.extend_from_slice(&chunk);
		}
		
		output_data.put_u32_le(terrain_data.len().try_into()?);
		output_data.extend_from_slice(&terrain_data);
		
		Ok(output_data.freeze())
	}
}