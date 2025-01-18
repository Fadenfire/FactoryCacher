use crate::dedup::ChunkKey;
use crate::utils;
use bytes::Bytes;
use hashlink::LinkedHashMap;
use log::{error, info, warn};
use std::collections::{HashMap, HashSet};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;

pub struct ChunkCache {
	inner: Mutex<ChunkCacheInner>,
}

struct ChunkCacheInner {
	raw_cache: RawChunkCache,
	pending_chunks: HashMap<ChunkKey, Arc<Semaphore>>,
	needs_saving: bool,
}

impl ChunkCache {
	pub fn new(max_size: u64) -> Self {
		Self {
			inner: Mutex::new(ChunkCacheInner {
				raw_cache: RawChunkCache::new(max_size),
				pending_chunks: HashMap::new(),
				needs_saving: false,
			}),
		}
	}
	
	pub async fn load_from_file(max_size: u64, cache_path: PathBuf) -> anyhow::Result<Self> {
		let raw_cache = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
			let mut raw_cache = RawChunkCache::new(max_size);
			
			if cache_path.exists() {
				read_chunk_cache(&mut raw_cache, &cache_path)?;
			}
			
			Ok(raw_cache)
		}).await??;
		
		Ok(Self {
			inner: Mutex::new(ChunkCacheInner {
				raw_cache,
				pending_chunks: HashMap::new(),
				needs_saving: false,
			}),
		})
	}
	
	pub fn start_writer(self: &Arc<Self>, cache_path: PathBuf, interval: Duration) {
		let arc_self = Arc::clone(self);
		
		tokio::spawn(async move {
			loop {
				tokio::time::sleep(interval).await;
				
				if let Err(err) = arc_self.try_save(cache_path.clone()).await {
					error!("Failed to save chunk cache: {}", err);
				}
			}
		});
	}
	
	async fn try_save(&self, cache_path: PathBuf) -> anyhow::Result<()> {
		let total_size;
		
		let cache_entries: Vec<_> = {
			let mut inner = self.inner.lock().expect("chunk cache poisoned");
			
			if !inner.needs_saving {
				return Ok(());
			}
			
			info!("Saving cache");
			
			inner.needs_saving = false;
			total_size = inner.raw_cache.total_size;
			
			inner.raw_cache.chunks.iter()
				.map(|(k, v)| (k.clone(), v.clone()))
				.collect()
		};
		
		let chunk_count = cache_entries.len();
		
		let compressed_size = tokio::task::spawn_blocking(move || -> anyhow::Result<u64> {
			let temp_path = cache_path.with_extension("tmp");
			
			write_chunk_cache(&cache_entries, &temp_path)?;
			
			let written_size = std::fs::metadata(&temp_path)?.len();
			std::fs::rename(&temp_path, &cache_path)?;
			
			Ok(written_size)
		}).await??;
		
		info!("Saved {} chunks to the cache ({}B, {}B compressed)", chunk_count,
			utils::abbreviate_number(total_size), utils::abbreviate_number(compressed_size));
		
		Ok(())
	}
	
	/// Gets all requested chunks, or builds a batch to be fetched.
	/// 
	/// All requested chunks currently in the cache will be placed into chunk_out.
	/// Any remaining chunks that aren't currently being fetched by another task will be bundled into a batch
	///  of at most batch_size chunks and returned. The caller can then fetch these and insert them into the cache by
	///  using the BatchChunkRequest's fulfill function.
	/// Finally, if all requested chunks are being fetched by other tasks, then wait for those tasks to complete and
	///  place the final chunks into chunk_out.
	/// 
	/// Returns None when all requests have been fulfilled.
	pub async fn get_chunks_batched(&self,
		chunks_requested: &mut Vec<ChunkKey>,
		chunk_out: &mut HashMap<ChunkKey, Bytes>,
		batch_size: usize,
	) -> Option<BatchChunkRequest> {
		let pending_requests = {
			let mut inner = self.inner.lock().unwrap();
			
			let mut batch_set = HashSet::with_capacity(batch_size);
			let mut batch = Vec::new();
			
			chunks_requested.retain(|&key| {
				let mut retain = true;
				
				// If the requested chunk is already in the cache, remove it from requested and output it.
				if let Some(chunk) = inner.raw_cache.get(&key) {
					chunk_out.insert(key, chunk.clone());
					
					retain = false;
				} else if !inner.pending_chunks.contains_key(&key) &&
					batch.len() < batch_size &&
					!batch_set.contains(&key)
				{
					// If the requested chunk is not in the cache, and it's not currently being requested, then add it to
					//  the batch and remove it from requested.
					batch.push(key);
					batch_set.insert(key);
					
					retain = false;
				}
				
				retain
			});
			
			// If we built a batch, then mark all chunks in the batch as pending and return the chunk keys to be
			//  fetched.
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
			
			// Otherwise, collect all chunks currently being fetched by somebody else and wait for them to finish.
			
			let mut pending_requests = Vec::new();
			
			chunks_requested.retain(|&key| {
				let mut retain = true;
				
				if let Some(event) = inner.pending_chunks.get(&key) {
					pending_requests.push((key, event.clone()));
					retain = false;
				}
				
				retain
			});
			
			if pending_requests.is_empty() {
				return None;
			}
			
			pending_requests
		};
		
		for (_key, event) in &pending_requests {
			let _ = event.acquire().await;
		}
		
		{
			let inner = self.inner.lock().unwrap();
			
			for (key, _event) in pending_requests {
				let chunk = inner.raw_cache.get(&key)
					.expect("waited on chunk, but chunk was not put in cache");
				
				chunk_out.insert(key, chunk.clone());
			}
		}
		
		None
	}
	
	// pub fn insert(&self, key: ChunkKey, chunk: Bytes) {
	// 	let mut inner = self.inner.lock().unwrap();
	// 	
	// 	inner.chunks.insert(key, chunk);
	// 	inner.pending_chunks.remove(&key);
	// }
	
	pub fn mark_dirty(&self) {
		let mut inner = self.inner.lock().unwrap();
		inner.needs_saving = true;
	}
	
	pub fn len(&self) -> usize {
		let inner = self.inner.lock().unwrap();
		inner.raw_cache.chunks.len()
	}
	
	pub fn total_size(&self) -> u64 {
		let inner = self.inner.lock().unwrap();
		inner.raw_cache.total_size
	}
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
				inner.raw_cache.insert(key, chunk.clone());
				inner.pending_chunks.remove(&key);
			}
		}
		
		self.event.close();
	}
}

struct RawChunkCache {
	chunks: LinkedHashMap<ChunkKey, Bytes>,
	total_size: u64,
	max_size: u64,
}

impl RawChunkCache {
	pub fn new(max_size: u64) -> Self {
		Self {
			chunks: LinkedHashMap::new(),
			total_size: 0,
			max_size,
		}
	}
	
	pub fn insert(&mut self, key: ChunkKey, chunk: Bytes) {
		self.total_size += chunk.len() as u64;
		
		if let Some(old_chunk) = self.chunks.insert(key, chunk) {
			warn!("Inserting chunk twice: {}", key.0);
			self.total_size -= old_chunk.len() as u64;
		}
		
		while self.total_size > self.max_size {
			let (_, evicted_chunk) = self.chunks.pop_front().unwrap();
			self.total_size -= evicted_chunk.len() as u64;
		}
	}
	
	pub fn get(&self, key: &ChunkKey) -> Option<&Bytes> {
		self.chunks.get(key)
	}
}

pub const CHUNK_CACHE_COMPRESSION_LEVEL: i32 = 8;

fn read_chunk_cache(cache: &mut RawChunkCache, cache_path: &Path) -> anyhow::Result<()> {
	let file = std::fs::File::open(cache_path)?;
	let mut decoder = zstd::Decoder::new(file)?;
	
	let mut u32_buf = [0u8; 4];
	
	decoder.read_exact(&mut u32_buf)?;
	let chunks_in_file = u32::from_le_bytes(u32_buf);
	
	for _ in 0..chunks_in_file {
		let mut chunk_key_bytes = [0; 32];
		decoder.read_exact(&mut chunk_key_bytes)?;
		
		let chunk_key = ChunkKey(blake3::Hash::from(chunk_key_bytes));
		
		decoder.read_exact(&mut u32_buf)?;
		let chunk_length = u32::from_le_bytes(u32_buf);
		
		if chunk_length > 20_000_000 {
			return Err(anyhow::anyhow!("Chunk length too large: {}", chunk_length));
		}
		
		let mut chunk_data = vec![0; chunk_length as usize];
		decoder.read_exact(&mut chunk_data)?;
		
		let data_hash = blake3::hash(&chunk_data);
		
		if data_hash != chunk_key.0 {
			error!("Chunk hash mismatch while loading cache, expected {}, got {}", chunk_key.0, data_hash);
			continue;
		}
		
		cache.insert(chunk_key, chunk_data.into());
	}
	
	Ok(())
}

fn write_chunk_cache(cache_entries: &[(ChunkKey, Bytes)], cache_path: &Path) -> anyhow::Result<()> {
	let file = std::fs::File::create(cache_path)?;
	let writer = BufWriter::new(file);
	let mut encoder = zstd::Encoder::new(writer, CHUNK_CACHE_COMPRESSION_LEVEL)?;
	
	encoder.write_all(&u32::try_from(cache_entries.len())
		.expect("Chunk count wouldn't fit into a u32")
		.to_le_bytes()
	)?;
	
	for (key, chunk) in cache_entries {
		encoder.write_all(key.0.as_bytes())?;
		
		encoder.write_all(&u32::try_from(chunk.len())
			.expect("Chunk size wouldn't fit into a u32")
			.to_le_bytes()
		)?;
		
		encoder.write_all(&chunk)?;
	}
	
	let mut writer = encoder.finish()?;
	writer.flush()?;
	
	Ok(())
}
