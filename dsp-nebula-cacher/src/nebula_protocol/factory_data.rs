use bytes::{Buf, BufMut, Bytes, BytesMut};
use std::collections::HashMap;
use futures_util::{pin_mut, StreamExt};
use common::dedup::{ChunkList, ChunkProvider};
use common::dedup;
use common::dedup::ChunkKey;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use crate::{lz4_frame_encoder, nebula_protocol};
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
	pub data_uncompressed_length: u64,
	pub data_chunk_list: ChunkList,
	pub terrain_data_length: u32,
	pub terrain_data_chunk_list: ChunkList,
}

impl FactoryDataPacket {
	pub fn deconstruct(
		header: FactoryDataHeader,
		mut packet_data: Bytes,
		all_chunks: &mut HashMap<ChunkKey, Bytes>
	) -> anyhow::Result<Self> {
		let (data_chunk_list, data_uncompressed_length) =
			nebula_protocol::deconstruct_lz4_data(&mut packet_data, header.data_length as usize, all_chunks)?;
		
		let terrain_data_length = packet_data.try_get_u32_le()?;
		let terrain_data = nebula_protocol::try_split_to(&mut packet_data, terrain_data_length as usize)?;
		let terrain_data_chunk_list = dedup::chunk_data(&terrain_data, all_chunks);
		
		Ok(Self {
			planet_id: header.planet_id,
			data_uncompressed_length,
			data_chunk_list,
			terrain_data_length,
			terrain_data_chunk_list,
		})
	}
	
	pub async fn reconstruct(self,
		chunks: &mut impl ChunkProvider,
		out_sink: &mpsc::Sender<Bytes>,
	) -> anyhow::Result<()> {
		let mut buf = BytesMut::new();
		
		buf.put_u64_le(FACTORY_DATA_PACKET_ID);
		buf.put_u32_le(self.planet_id);
		buf.put_u32_le(lz4_frame_encoder::framed_size(self.data_uncompressed_length).try_into()?);
		out_sink.send(buf.split().freeze()).await?;
		
		nebula_protocol::reconstruct_lz4_data(&self.data_chunk_list, chunks, out_sink).await?;
		
		buf.put_u32_le(self.terrain_data_length);
		out_sink.send(buf.split().freeze()).await?;
		
		let reconstructed_chunks = dedup::reconstruct_data(&self.terrain_data_chunk_list, chunks);
		pin_mut!(reconstructed_chunks);
		
		while let Some(chunk) = reconstructed_chunks.next().await {
			out_sink.send(chunk?).await?;
		}
		
		Ok(())
	}
}