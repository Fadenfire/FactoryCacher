use bytes::{Buf, BufMut, Bytes, BytesMut};
use std::collections::HashMap;
use common::dedup::{ChunkList, ChunkProvider};
use common::dedup::ChunkKey;
use serde::{Deserialize, Serialize};
use crate::nebula_protocol;
use crate::nebula_protocol::GAME_DATA_PACKET_ID;

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
	
	pub fn approx_size(&self) -> usize {
		self.data_length as usize
	}
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GlobalGameDataPacket {
	pub data_type: u8,
	pub data_chunk_list: ChunkList,
}

impl GlobalGameDataPacket {
	pub fn deconstruct(
		header: GlobalGameDataHeader,
		mut packet_data: Bytes,
		all_chunks: &mut HashMap<ChunkKey, Bytes>
	) -> anyhow::Result<Self> {
		let data_chunk_list =
			nebula_protocol::deconstruct_lz4_data(&mut packet_data, header.data_length as usize, all_chunks)?;
		
		Ok(Self {
			data_type: header.data_type,
			data_chunk_list,
		})
	}
	
	pub async fn reconstruct(self, chunks: &mut impl ChunkProvider) -> anyhow::Result<Bytes> {
		let mut output_data = BytesMut::new();
		
		let data = nebula_protocol::reconstruct_lz4_data(&self.data_chunk_list, chunks).await?;
		
		output_data.put_u64_le(GAME_DATA_PACKET_ID);
		output_data.put_u8(self.data_type);
		output_data.put_u32_le(data.len().try_into()?);
		output_data.put(data);
		
		Ok(output_data.freeze())
	}
}