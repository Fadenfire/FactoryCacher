use bytes::{Buf, BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use common::dedup::ChunkKey;
use std::collections::HashMap;
use common::chunks::ChunkProvider;
use crate::nebula_protocol;
use crate::nebula_protocol::DYSON_SPHERE_DATA_PACKET_ID;

#[derive(Debug, Clone)]
pub struct DysonSphereDataHeader {
	pub star_index: u32,
	pub data_length: u32,
}

impl DysonSphereDataHeader {
	pub fn decode(mut buf: impl Buf) -> anyhow::Result<Self> {
		Ok(Self {
			star_index: buf.try_get_u32_le()?,
			data_length: buf.try_get_u32_le()?,
		})
	}
	
	pub fn approx_size(&self) -> usize {
		self.data_length as usize
	}
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DysonSphereDataPacket {
	pub star_index: u32,
	pub data_chunk_list: Vec<ChunkKey>,
	pub event_type: u32,
}

impl DysonSphereDataPacket {
	pub fn deconstruct(
		header: DysonSphereDataHeader,
		mut packet_data: Bytes,
		all_chunks: &mut HashMap<ChunkKey, Bytes>
	) -> anyhow::Result<Self> {
		let data_chunk_list =
			nebula_protocol::deconstruct_lz4_data(&mut packet_data, header.data_length as usize, all_chunks)?;
		
		let event_type = packet_data.try_get_u32_le()?;
		
		Ok(Self {
			star_index: header.star_index,
			data_chunk_list,
			event_type
		})
	}
	
	pub fn required_chunks(&self) -> Vec<ChunkKey> {
		self.data_chunk_list.clone()
	}
	
	pub async fn reconstruct(self, chunks: &mut impl ChunkProvider) -> anyhow::Result<Bytes> {
		let mut output_data = BytesMut::new();
		
		let data = nebula_protocol::reconstruct_lz4_data(&self.data_chunk_list, chunks).await?;
		
		output_data.put_u64_le(DYSON_SPHERE_DATA_PACKET_ID);
		output_data.put_u32_le(self.star_index);
		output_data.put_u32_le(data.len().try_into()?);
		output_data.put(data);
		output_data.put_u32_le(self.event_type);
		
		Ok(output_data.freeze())
	}
}