use std::collections::HashMap;
use bytes::Bytes;
use crate::dedup::ChunkKey;

pub trait ChunkProvider {
	async fn get_chunk(&mut self, key: ChunkKey) -> anyhow::Result<Option<Bytes>>;
}

impl ChunkProvider for HashMap<ChunkKey, Bytes> {
	async fn get_chunk(&mut self, key: ChunkKey) -> anyhow::Result<Option<Bytes>> {
		let chunk = self.get(&key).cloned();
		
		Ok(chunk)
	}
}