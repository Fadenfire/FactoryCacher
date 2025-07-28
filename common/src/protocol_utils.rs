use crate::chunk_cache::ChunkCache;
use crate::dedup::ChunkKey;
use crate::utils;
use bytes::{Bytes, BytesMut};
use log::info;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use crate::chunks::ChunkProvider;

const ZSTD_COMPRESSION_LEVEL: i32 = 9;
const MESSAGE_SIZE_LIMIT: usize = 20_000_000;

pub fn encode_message<T: Serialize>(message: &T) -> anyhow::Result<Bytes> {
	let mut data: Vec<u8> = Vec::new();
	
	let mut encoder = zstd::Encoder::new(&mut data, ZSTD_COMPRESSION_LEVEL)?;
	rmp_serde::encode::write(&mut encoder, message)?;
	encoder.finish()?;
	
	Ok(data.into())
}

pub async fn encode_message_async<T: Serialize + Send + 'static>(message: T) -> anyhow::Result<Bytes> {
	tokio::task::spawn_blocking(move || encode_message(&message)).await?
}

pub fn decode_message<T: DeserializeOwned>(msg_data: &[u8]) -> anyhow::Result<T> {
	let decoder = zstd::Decoder::new(msg_data)?;
	
	Ok(rmp_serde::decode::from_read(decoder)?)
}

pub async fn decode_message_async<T: DeserializeOwned + Send + 'static>(msg_data: Bytes) -> anyhow::Result<T> {
	tokio::task::spawn_blocking(move || decode_message::<T>(&msg_data)).await?
}

pub async fn write_message<W: AsyncWrite + Unpin>(io: &mut W, msg_data: Bytes) -> anyhow::Result<()> {
	if msg_data.len() > MESSAGE_SIZE_LIMIT {
		panic!("Message size exceeded limit");
	}
	
	io.write_u32_le(msg_data.len() as u32).await?;
	io.write_all(&msg_data).await?;
	
	Ok(())
}

pub async fn read_message<R: AsyncRead + Unpin>(io: &mut R, buffer: &mut BytesMut) -> anyhow::Result<Bytes> {
	let msg_size = io.read_u32_le().await? as usize;
	
	if msg_size > MESSAGE_SIZE_LIMIT {
		panic!("Message size exceeded limit");
	}
	
	buffer.resize(msg_size, 0);
	io.read_exact(buffer).await?;
	
	Ok(buffer.split().freeze())
}

#[derive(Deserialize, Serialize)]
pub struct RequestChunksMessage {
	pub requested_chunks: Vec<ChunkKey>,
}

#[derive(Deserialize, Serialize)]
pub struct SendChunksMessage {
	pub chunks: Vec<Bytes>,
}

pub async fn provide_chunks_as_requested(
	send_stream: &mut quinn::SendStream,
	recv_stream: &mut quinn::RecvStream,
	chunks: &HashMap<ChunkKey, Bytes>,
) -> anyhow::Result<usize> {
	let mut buf = BytesMut::new();
	let mut total_transferred = 0;
	
	while let Ok(request_data) = read_message(recv_stream, &mut buf).await {
		let request: RequestChunksMessage = decode_message_async(request_data).await?;
		
		let response = SendChunksMessage {
			chunks: request.requested_chunks.iter()
				.map(|&key| {
					chunks.get(&key)
						.ok_or_else(|| anyhow::anyhow!("Client requested chunk that we don't have"))
						.cloned()
				})
				.collect::<anyhow::Result<_>>()?,
		};
		
		let response_data = encode_message_async(response).await?;
		total_transferred += response_data.len();
		
		info!("Sending batch of {} chunks, size: {}B",
			request.requested_chunks.len(),
			utils::abbreviate_number(response_data.len() as u64)
		);
		
		write_message(send_stream, response_data).await?;
	}
	
	Ok(total_transferred)
}

pub struct ChunkFetcherProvider<'a> {
	chunk_cache: &'a ChunkCache,
	send_stream: &'a mut quinn::SendStream,
	recv_stream: &'a mut quinn::RecvStream,
	
	local_cache: HashMap<ChunkKey, Bytes>,
	chunks_remaining: Vec<ChunkKey>,
	total_transferred: usize,
	
	buffer: BytesMut,
}

impl<'a> ChunkFetcherProvider<'a> {
	pub fn new(
		chunk_cache: &'a ChunkCache,
		send_stream: &'a mut quinn::SendStream,
		recv_stream: &'a mut quinn::RecvStream,
	) -> Self {
		Self {
			chunk_cache,
			send_stream,
			recv_stream,
			
			local_cache: HashMap::new(),
			chunks_remaining: Vec::new(),
			total_transferred: 0,
			
			buffer: BytesMut::new(),
		}
	}
	
	pub fn set_chunks_remaining(&mut self, chunks_remaining: Vec<ChunkKey>) {
		self.chunks_remaining = chunks_remaining;
	}
	
	pub fn total_transferred(&self) -> usize {
		self.total_transferred
	}
}

impl<'a> ChunkProvider for ChunkFetcherProvider<'a> {
	async fn get_chunk(&mut self, key: ChunkKey) -> anyhow::Result<Option<Bytes>> {
		loop {
			if let Some(chunk) = self.local_cache.get(&key) {
				return Ok(Some(chunk.clone()));
			}
			
			if self.chunks_remaining.is_empty() {
				return Ok(None);
			}
			
			self.fetch_chunk_batch().await?;
		}
	}
}

impl<'a> ChunkFetcherProvider<'a> {
	async fn fetch_chunk_batch(&mut self) -> anyhow::Result<()> {
		let Some(batch) = self.chunk_cache.get_chunks_batched(&mut self.chunks_remaining, &mut self.local_cache, 512).await
			else { return Ok(()); };
		
		let request_data = encode_message_async(RequestChunksMessage {
			requested_chunks: batch.batch_keys().to_vec(),
		}).await?;
		
		self.total_transferred += request_data.len();
		
		write_message(self.send_stream, request_data).await?;
		
		let response_data = read_message(self.recv_stream, &mut self.buffer).await?;
		
		self.total_transferred += response_data.len();
		
		info!("Received batch of {} chunks, size: {}B",
			batch.batch_keys().len(),
			utils::abbreviate_number(response_data.len() as u64)
		);
		
		let response: SendChunksMessage = decode_message_async(response_data).await?;
		
		for (&key, chunk) in batch.batch_keys().iter().zip(response.chunks.iter()) {
			let data_hash = blake3::hash(&chunk);
			
			if data_hash != key.0 {
				return Err(anyhow::anyhow!("Chunk hash mismatch for {:?}", key));
			}
			
			self.local_cache.insert(key, chunk.clone());
		}
		
		batch.fulfill(&response.chunks);
		
		Ok(())
	}
}