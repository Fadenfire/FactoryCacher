use crate::chunker::Chunker;
use crate::factorio_protocol::{FACTORIO_CRC, TRANSFER_BLOCK_SIZE};
use crate::zip_writer::ZipWriter;
use bytes::{BufMut, Bytes, BytesMut};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{Cursor, Read};
use zip::ZipArchive;

pub const RECONSTRUCT_DEFLATE_LEVEL: u8 = 0;

#[derive(Deserialize, Serialize)]
pub struct FactorioWorldDescription {
	pub files: Vec<FactorioFileDescription>,
	pub aux_data: Bytes,
	pub world_size: u32,
	pub world_block_length: u32,
	pub aux_block_length: u32,
	pub total_size: u32,
	pub reconstructed_crc: u32,
}

#[derive(Deserialize, Serialize)]
pub struct FactorioFileDescription {
	pub file_type: FactorioFileType,
	pub file_name: String,
	pub content_size: u64,
	pub content_chunks: Vec<ChunkKey>,
}

#[derive(Debug, Eq, PartialEq, Copy, Clone, Deserialize, Serialize)]
pub enum FactorioFileType {
	Normal,
	Zlib,
}

pub struct FactorioFile<'a> {
	pub file_type: FactorioFileType,
	pub data: Cow<'a, [u8]>,
}

pub fn deconstruct_world(
	world_data: &[u8],
	aux_data: &[u8]
) -> anyhow::Result<(FactorioWorldDescription, HashMap<ChunkKey, Bytes>)> {
	let mut zip_reader = ZipArchive::new(Cursor::new(&world_data))?;
	
	let mut zip_writer = ZipWriter::new();
	let mut crc_hasher = FACTORIO_CRC.digest();
	let mut world_size: u32 = 0;
	
	let mut chunks = HashMap::new();
	let mut files = Vec::new();
	
	let mut buf = Vec::new();
	
	let mut add_data = |data: &[u8]| {
		crc_hasher.update(data);
		world_size += data.len() as u32;
	};
	
	for i in 0..zip_reader.len() {
		let mut zip_file = zip_reader.by_index(i)?;
		
		buf.clear();
		zip_file.read_to_end(&mut buf)?;
		
		let decoded_file = decode_factorio_file(zip_file.name(), &buf)?;
		
		files.push(chunk_file(zip_file.name(), &decoded_file, &mut chunks)?);
		
		let re_encoded_data = encode_factorio_file(&decoded_file);
		
		add_data(&zip_writer.encode_file_header(zip_file.name(), &re_encoded_data));
		add_data(&re_encoded_data);
	}
	
	add_data(&zip_writer.encode_central_directory());
	
	let world_block_length = (world_size + TRANSFER_BLOCK_SIZE - 1) / TRANSFER_BLOCK_SIZE;
	let aux_block_length = (aux_data.len() as u32 + TRANSFER_BLOCK_SIZE - 1) / TRANSFER_BLOCK_SIZE;
	
	let total_size = (world_block_length + aux_block_length) * TRANSFER_BLOCK_SIZE;
	
	crc_hasher.update(aux_data);
	let reconstructed_crc = crc_hasher.finalize();
	
	let world = FactorioWorldDescription {
		files,
		aux_data: aux_data.to_vec().into(),
		world_size,
		world_block_length,
		aux_block_length,
		total_size,
		reconstructed_crc,
	};
	
	Ok((world, chunks))
}

pub struct WorldReconstructor {
	zip_writer: ZipWriter,
	current_size: usize,
}

pub struct NeedsMoreData;

impl WorldReconstructor {
	pub fn new() -> Self {
		Self {
			zip_writer: ZipWriter::new(),
			current_size: 0,
		}
	}
	
	pub fn reconstruct_world_file(
		&mut self,
		file_desc: &FactorioFileDescription,
		chunks: &HashMap<ChunkKey, Bytes>,
		buf: &mut BytesMut
	) -> Result<(Bytes, Bytes), NeedsMoreData> {
		buf.clear();
		
		for &chunk_key in &file_desc.content_chunks {
			if let Some(chunk) = chunks.get(&chunk_key) {
				buf.put_slice(chunk);
			} else {
				return Err(NeedsMoreData);
			}
		}
		
		let file = FactorioFile {
			file_type: file_desc.file_type,
			data: Cow::Borrowed(&buf),
		};
		
		let file_data = encode_factorio_file(&file);
		let header = self.zip_writer.encode_file_header(&file_desc.file_name, &file_data);
		
		self.current_size += header.len();
		self.current_size += file_data.len();
		
		Ok((header, file_data.into_owned().into()))
	}
	
	pub fn finalize_world_file(mut self, world_desc: &FactorioWorldDescription) -> anyhow::Result<Bytes> {
		let mut buf: BytesMut = self.zip_writer.encode_central_directory().into();
		
		let old_size = self.current_size;
		self.current_size += buf.len();
		
		if self.current_size != world_desc.world_size as usize {
			return Err(anyhow::anyhow!("Client world size doesn't match server client: {}, server: {}",
				self.current_size, world_desc.world_size));
		}
		
		let world_aligned_length = (world_desc.world_block_length * TRANSFER_BLOCK_SIZE) as usize;
		let aux_aligned_length = (world_desc.aux_block_length * TRANSFER_BLOCK_SIZE) as usize;
		
		buf.resize(world_aligned_length - self.current_size + buf.len(), 0);
		
		buf.put_slice(&world_desc.aux_data);
		buf.resize(aux_aligned_length - world_desc.aux_data.len() + buf.len(), 0);
		
		assert_eq!((old_size + buf.len()) % TRANSFER_BLOCK_SIZE as usize, 0);
		
		Ok(buf.freeze())
	}
}

pub fn decode_factorio_file<'a>(file_name: &str, file_data: &'a [u8]) -> anyhow::Result<FactorioFile<'a>> {
	let name = file_name.rsplit_once('/').map(|(_, last)| last).unwrap_or(file_name);
	
	if name.strip_prefix("level.dat").is_some_and(|suffix| suffix.chars().all(|c| c.is_ascii_digit())) {
		let uncompressed_data = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(file_data, 20_000_000)?;
		
		Ok(FactorioFile {
			file_type: FactorioFileType::Zlib,
			data: uncompressed_data.into(),
		})
	} else {
		Ok(FactorioFile {
			file_type: FactorioFileType::Normal,
			data: file_data.into(),
		})
	}
}

pub fn encode_factorio_file<'a>(file: &'a FactorioFile<'a>) -> Cow<[u8]> {
	match file.file_type {
		FactorioFileType::Normal => Cow::Borrowed(&file.data),
		FactorioFileType::Zlib => {
			let data = miniz_oxide::deflate::compress_to_vec_zlib(&file.data, RECONSTRUCT_DEFLATE_LEVEL);
			
			Cow::Owned(data)
		},
	}
}

pub fn chunk_file(file_name: &str, file: &FactorioFile, chunks: &mut HashMap<ChunkKey, Bytes>) -> anyhow::Result<FactorioFileDescription> {
	let chunker = Chunker::new(&file.data);
	
	let mut content_chunks = Vec::new();
	
	for chunk in chunker {
		let hash = ChunkKey(blake3::hash(chunk));
		
		content_chunks.push(hash);
		chunks.insert(hash, chunk.to_vec().into());
	}
	
	Ok(FactorioFileDescription {
		file_type: file.file_type,
		file_name: file_name.to_owned(),
		content_size: file.data.len() as u64,
		content_chunks,
	})
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct ChunkKey(pub blake3::Hash);

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
		serializer.serialize_bytes(self.0.as_bytes())
	}
}