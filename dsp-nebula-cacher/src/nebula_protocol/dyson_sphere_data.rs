use bytes::{Buf, BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use common::dedup::{ChunkKey, ChunkList};
use std::collections::HashMap;
use tokio::sync::mpsc;
use common::dedup::ChunkProvider;
use crate::{lz4_frame_encoder, nebula_protocol};
use crate::nebula_protocol::{DYSON_SPHERE_DATA_PACKET_ID};

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
	pub data_uncompressed_length: u64,
	pub data_chunk_list: ChunkList,
	pub event_type: u32,
}

impl DysonSphereDataPacket {
	pub fn deconstruct(
		header: DysonSphereDataHeader,
		mut packet_data: Bytes,
		all_chunks: &mut HashMap<ChunkKey, Bytes>
	) -> anyhow::Result<Self> {
		let (data_chunk_list, data_uncompressed_length) =
			nebula_protocol::deconstruct_lz4_data(&mut packet_data, header.data_length as usize, all_chunks)?;
		
		let event_type = packet_data.try_get_u32_le()?;
		
		Ok(Self {
			star_index: header.star_index,
			data_uncompressed_length,
			data_chunk_list,
			event_type
		})
	}
	
	pub async fn reconstruct(self,
		chunks: &mut impl ChunkProvider,
		out_sink: &mpsc::Sender<Bytes>,
	) -> anyhow::Result<()> {
		let mut buf = BytesMut::new();
		
		buf.put_u64_le(DYSON_SPHERE_DATA_PACKET_ID);
		buf.put_u32_le(self.star_index);
		buf.put_u32_le(lz4_frame_encoder::framed_size(self.data_uncompressed_length).try_into()?);
		out_sink.send(buf.split().freeze()).await?;
		
		nebula_protocol::reconstruct_lz4_data(&self.data_chunk_list, chunks, out_sink).await?;
		
		buf.put_u32_le(self.event_type);
		out_sink.send(buf.split().freeze()).await?;
		
		Ok(())
	}
}