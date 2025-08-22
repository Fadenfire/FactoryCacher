use bytes::{Buf, BufMut, Bytes, BytesMut};
use std::collections::HashMap;
use common::dedup::{ChunkList, ChunkProvider};
use common::dedup::ChunkKey;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use crate::{lz4_frame_encoder, nebula_protocol};
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
	pub data_uncompressed_length: u64,
	pub data_chunk_list: ChunkList,
}

impl GlobalGameDataPacket {
	pub fn deconstruct(
		header: GlobalGameDataHeader,
		mut packet_data: Bytes,
		all_chunks: &mut HashMap<ChunkKey, Bytes>
	) -> anyhow::Result<Self> {
		let (data_chunk_list, data_uncompressed_length) =
			nebula_protocol::deconstruct_lz4_data(&mut packet_data, header.data_length as usize, all_chunks)?;
		
		Ok(Self {
			data_type: header.data_type,
			data_uncompressed_length,
			data_chunk_list,
		})
	}
	
	pub async fn reconstruct(self,
		chunks: &mut impl ChunkProvider,
		out_sink: &mpsc::Sender<Bytes>,
	) -> anyhow::Result<()> {
		let mut buf = BytesMut::new();
		
		buf.put_u64_le(GAME_DATA_PACKET_ID);
		buf.put_u8(self.data_type);
		buf.put_u32_le(lz4_frame_encoder::framed_size(self.data_uncompressed_length).try_into()?);
		out_sink.send(buf.freeze()).await?;
		
		nebula_protocol::reconstruct_lz4_data(&self.data_chunk_list, chunks, out_sink).await?;
		
		Ok(())
	}
}