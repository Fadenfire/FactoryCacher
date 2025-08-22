use crate::chunker::Chunker;
use async_stream::try_stream;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use futures_core::Stream;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::mem::swap;

const MIN_CHUNKS_IN_LIST: usize = 128;
const AVG_CHUNKS_IN_LIST: u32 = 512;
const MAX_CHUNKS_IN_LIST: usize = 2048;

fn is_split_point(hash: ChunkKey, num_nodes: usize) -> bool {
	if num_nodes < MIN_CHUNKS_IN_LIST {
		return false;
	} else if num_nodes >= MAX_CHUNKS_IN_LIST {
		return true;
	}
	
	let hash_u32 = u32::from_be_bytes(hash.as_bytes()[0..4].try_into().unwrap());
	
	hash_u32 % AVG_CHUNKS_IN_LIST == 0
}

pub fn chunk_data(data: &[u8], chunks: &mut HashMap<ChunkKey, Bytes>) -> ChunkList {
	let chunker = Chunker::new(data);
	
	let mut tree_levels = Vec::new();
	// Push on initial leaf node
	tree_levels.push(ChunkList::new_empty(true));
	
	for chunk in chunker {
		let mut hash = ChunkKey(blake3::hash(chunk));
		
		tree_levels[0].push_chunk(hash);
		chunks.insert(hash, Bytes::copy_from_slice(chunk));
		
		let mut level = 0;
		
		// While the hash of the node from the previous level is a split point, split the current level
		// Since we're at a split point for this level, the current node is complete
		while is_split_point(hash, tree_levels[level].chunks().len()) {
			let serialized_chunk_list = tree_levels[level].encode();
			
			hash = ChunkKey(blake3::hash(&serialized_chunk_list));
			
			// Add a new level to the tree if needed
			if tree_levels.len() <= level + 1 {
				tree_levels.push(ChunkList::new_empty(false));
			}
			
			// Append the current node to the next level
			tree_levels[level + 1].push_chunk(hash);
			
			// Add the serialized current node to the chunk list
			chunks.insert(hash, serialized_chunk_list);
			
			// Replace the node for the current level with a new fresh node
			tree_levels[level] = ChunkList::new_empty(tree_levels[level].is_leaf());
			
			// Progress to the next level
			level += 1;
		}
	}
	
	// Finish tree
	
	for level in 0..(tree_levels.len() - 1) {
		if tree_levels[level].chunks.is_empty() { continue; }
		
		let serialized_chunk_list = tree_levels[level].encode();
		let hash = ChunkKey(blake3::hash(&serialized_chunk_list));
		
		// Append the current node to the next level
		tree_levels[level + 1].chunks.push(hash);
		
		// Add the serialized current node to the chunk list
		chunks.insert(hash, serialized_chunk_list);
	}
	
	tree_levels.into_iter().last().unwrap()
}

pub const MINIMIZE_THRESHOLD: usize = 64;

pub fn minimize_chunk_list(chunk_list: ChunkList, chunks: &mut HashMap<ChunkKey, Bytes>) -> ChunkList {
	if chunk_list.chunks().len() < MINIMIZE_THRESHOLD {
		return chunk_list;
	}
	
	let serialized_chunk_list = chunk_list.encode();
	let hash = ChunkKey(blake3::hash(&serialized_chunk_list));
	
	chunks.insert(hash, serialized_chunk_list);
	
	let mut root_chunk_list = ChunkList::new_empty(false);
	root_chunk_list.push_chunk(hash);
	
	root_chunk_list
}

pub async fn prefetch_chunks(
	chunk_lists: Vec<ChunkList>,
	chunk_provider: &mut impl ChunkProvider,
) -> anyhow::Result<()> {
	let mut current_level = Vec::new();
	let mut next_level = chunk_lists;
	
	while !next_level.is_empty() {
		swap(&mut current_level, &mut next_level);
		next_level.clear();
		
		chunk_provider.prefetch(
			current_level.iter()
				.flat_map(|chunk_list| chunk_list.chunks().iter())
				.copied()
		);
		
		for chunk_list in current_level.drain(..) {
			if !chunk_list.is_leaf() {
				for &child_chunk_key in chunk_list.chunks() {
					let child_chunk = chunk_provider.get_chunk(child_chunk_key).await?;
					let child_chunk_list = ChunkList::decode(child_chunk)?;
					
					next_level.push(child_chunk_list);
				}
			}
		}
	}
	
	Ok(())
}

pub fn reconstruct_data(
	chunk_list: &ChunkList,
	chunk_provider: &mut impl ChunkProvider,
) -> impl Stream<Item = anyhow::Result<Bytes>> {
	chunk_provider.prefetch(chunk_list.chunks().iter().copied());
	
	try_stream! {
		for &chunk_key in chunk_list.chunks() {
			let chunk = chunk_provider.get_chunk(chunk_key).await?;
			
			if chunk_list.is_leaf() {
				yield chunk;
			} else {
				let child_chunk_list = ChunkList::decode(chunk)?;
				let child_chunks = Box::pin(reconstruct_data(&child_chunk_list, chunk_provider));
				
				for await child_chunk in child_chunks {
					yield child_chunk?;
				}
			}
		}
	}
}

pub trait ChunkProvider {
	fn prefetch(&mut self, chunks: impl IntoIterator<Item = ChunkKey>);
	
	fn get_chunk(&mut self, key: ChunkKey) -> impl Future<Output = anyhow::Result<Bytes>>;
}

impl ChunkProvider for HashMap<ChunkKey, Bytes> {
	fn prefetch(&mut self, _chunks: impl IntoIterator<Item = ChunkKey>) {}
	
	async fn get_chunk(&mut self, key: ChunkKey) -> anyhow::Result<Bytes> {
		let chunk = self.get(&key).expect("No such key").clone();
		
		Ok(chunk)
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkList {
	is_leaf: bool,
	chunks: Vec<ChunkKey>,
}

impl ChunkList {
	pub fn new_empty(is_leaf: bool) -> Self {
		Self {
			is_leaf,
			chunks: Vec::new(),
		}
	}
	
	pub fn is_leaf(&self) -> bool {
		self.is_leaf
	}
	
	pub fn chunks(&self) -> &[ChunkKey] {
		&self.chunks
	}
	
	pub fn push_chunk(&mut self, chunk: ChunkKey) {
		self.chunks.push(chunk);
	}
	
	pub fn decode(mut buf: impl Buf) -> anyhow::Result<Self> {
		let is_leaf = buf.try_get_u8()? != 0;
		let chunk_count = buf.try_get_u32()? as usize;
		
		let mut chunks = Vec::with_capacity(chunk_count);
		
		for _ in 0..chunk_count {
			let mut hash_bytes = [0u8; blake3::OUT_LEN];
			buf.try_copy_to_slice(&mut hash_bytes)?;
			
			chunks.push(ChunkKey(blake3::Hash::from_bytes(hash_bytes)));
		}
		
		Ok(Self {
			is_leaf,
			chunks,
		})
	}
	
	pub fn encode(&self) -> Bytes {
		let mut buf = BytesMut::new();
		
		buf.put_u8(self.is_leaf as u8);
		buf.put_u32(self.chunks.len().try_into().expect("ChunkList is too big"));
		
		for chunk_key in &self.chunks {
			buf.put_slice(chunk_key.as_bytes());
		}
		
		buf.freeze()
	}
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct ChunkKey(pub blake3::Hash);

impl ChunkKey {
	pub fn as_bytes(&self) -> &[u8; 32] {
		self.0.as_bytes()
	}
}

impl<'de> Deserialize<'de> for ChunkKey {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where D: Deserializer<'de>
	{
		let data: [u8; 32] = serde_bytes::deserialize(deserializer)?;
		
		Ok(ChunkKey(blake3::Hash::from(data)))
	}
}

impl Serialize for ChunkKey {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where S: Serializer
	{
		serializer.serialize_bytes(self.as_bytes())
	}
}
