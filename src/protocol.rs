use std::time::Duration;
use crate::dedup::{ChunkKey, FactorioWorldDescription};
use bytes::{BufMut, Bytes, BytesMut};
use quinn_proto::coding::Codec;
use quinn_proto::VarInt;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use crate::factorio_protocol::FactorioWorldMetadata;

pub const UDP_PEER_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Eq, PartialEq)]
pub struct Datagram {
	pub peer_id: VarInt,
	pub data: Bytes,
}

impl Datagram {
	pub fn new(peer_id: VarInt, data: Bytes) -> Self {
		Self {
			peer_id,
			data,
		}
	}
	
	pub fn decode(mut data: Bytes) -> anyhow::Result<Self> {
		let peer_id = VarInt::decode(&mut data)?;
		
		Ok(Self {
			peer_id,
			data,
		})
	}
	
	pub fn encode(&self, buffer: &mut BytesMut) {
		self.peer_id.encode(buffer);
		buffer.put_slice(&self.data);
	}
}

const ZSTD_COMPRESSION_LEVEL: i32 = 11;
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
pub struct WorldReadyMessage {
	pub world: FactorioWorldDescription,
	pub old_info: FactorioWorldMetadata,
	pub new_info: FactorioWorldMetadata,
}

#[derive(Deserialize, Serialize)]
pub struct RequestChunksMessage {
	pub requested_chunks: Vec<ChunkKey>,
}

#[derive(Deserialize, Serialize)]
pub struct SendChunksMessage {
	pub chunks: Vec<Bytes>,
}
