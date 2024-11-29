use crate::dedup::ChunkKey;
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

pub struct ChunkCache {
	inner: Mutex<ChunkCacheInner>,
}

struct ChunkCacheInner {
	chunks: HashMap<ChunkKey, Bytes>,
	pending_chunks: HashMap<ChunkKey, Arc<Semaphore>>,
}

impl ChunkCache {
	pub fn new() -> Self {
		Self {
			inner: Mutex::new(ChunkCacheInner {
				chunks: HashMap::new(),
				pending_chunks: HashMap::new(),
			})
		}
	}
	
	// pub async fn get_chunk(&self, key: ChunkKey) -> ChunkCacheResult {
	// 	let waiter = {
	// 		let mut inner = self.inner.lock().unwrap();
	// 		
	// 		if let Some(chunk) = inner.chunks.get(&key) {
	// 			return ChunkCacheResult::Present(chunk.clone());
	// 		}
	// 		
	// 		if let Some(waiter) = inner.pending_chunks.get(&key) {
	// 			waiter.clone()
	// 		} else {
	// 			let notify = Arc::new(Notify::new());
	// 			
	// 			inner.pending_chunks.insert(key, notify.clone());
	// 			
	// 			return ChunkCacheResult::Absent(ChunkHandle {
	// 				notify,
	// 				chunk_key: key,
	// 				cache: self,
	// 			});
	// 		}
	// 	};
	// 	
	// 	waiter.notified().await;
	// 	
	// 	let inner = self.inner.lock().unwrap();
	// 	let chunk = inner.chunks.get(&key)
	// 		.expect("Notified that chunk was inserted, but no chunk found")
	// 		.clone();
	// 	
	// 	ChunkCacheResult::Present(chunk)
	// }
	
	pub async fn get_chunks_batched(&self,
		chunks_requested: &mut Vec<ChunkKey>,
		chunk_out: &mut HashMap<ChunkKey, Bytes>,
		batch_size: usize,
	) -> Option<BatchChunkRequest> {
		let waitables = {
			let mut inner = self.inner.lock().unwrap();
			
			let mut batch = Vec::new();
			
			chunks_requested.retain(|&key| {
				let mut retain = true;
				
				if let Some(chunk) = inner.chunks.get(&key) {
					chunk_out.insert(key, chunk.clone());
					retain = false;
				} else if !inner.pending_chunks.contains_key(&key) && batch.len() < batch_size {
					batch.push(key);
					retain = false;
				}
				
				retain
			});
			
			if !batch.is_empty() {
				let event = Arc::new(Semaphore::new(0));
				
				for &key in &batch {
					inner.pending_chunks.insert(key, event.clone());
				}
				
				return Some(BatchChunkRequest {
					event,
					batch_keys: batch,
					cache: self,
				});
			}
			
			let mut waitables = Vec::new();
			
			chunks_requested.retain(|&key| {
				let mut retain = true;
				
				if let Some(event) = inner.pending_chunks.get(&key) {
					waitables.push(event.clone());
					retain = false;
				}
				
				retain
			});
			
			if waitables.is_empty() {
				return None;
			}
			
			waitables
		};
		
		for event in &waitables {
			let _ = event.acquire().await;
		}
		
		{
			let inner = self.inner.lock().unwrap();
			
			chunks_requested.retain(|&key| {
				let mut retain = true;
				
				if let Some(chunk) = inner.chunks.get(&key) {
					chunk_out.insert(key, chunk.clone());
					retain = false;
				}
				
				retain
			});
		}
		
		None
	}
	
	// pub fn insert(&self, key: ChunkKey, chunk: Bytes) {
	// 	let mut inner = self.inner.lock().unwrap();
	// 	
	// 	inner.chunks.insert(key, chunk);
	// 	inner.pending_chunks.remove(&key);
	// }
}

pub struct BatchChunkRequest<'a> {
	event: Arc<Semaphore>,
	batch_keys: Vec<ChunkKey>,
	cache: &'a ChunkCache,
}

impl<'a> BatchChunkRequest<'a> {
	pub fn batch_keys(&self) -> &[ChunkKey] {
		&self.batch_keys
	}
	
	pub fn fulfill(self, chunks: &[Bytes]) {
		assert_eq!(self.batch_keys.len(), chunks.len());
		
		{
			let mut inner = self.cache.inner.lock().unwrap();
			
			for (&key, chunk) in self.batch_keys.iter().zip(chunks.iter()) {
				inner.chunks.insert(key, chunk.clone());
				inner.pending_chunks.remove(&key);
			}
		}
		
		self.event.close();
	}
}
