use std::borrow::Cow;
use crate::chunker::Chunker;
use crate::io_utils::BufExt;
use bytes::{BufMut, Bytes, BytesMut};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::io::{Cursor, Read};
use zip::ZipArchive;
use crate::factorio_protocol::{FACTORIO_CRC, TRANSFER_BLOCK_SIZE};
use crate::zip_writer::ZipWriter;

pub const RECONSTRUCT_DEFLATE_LEVEL: u8 = 3;

#[derive(Deserialize, Serialize)]
pub struct FactorioWorldDescription {
	pub files: Vec<FactorioFileDescription>,
	pub aux_data: Bytes,
	pub reconstructed_size: u32,
	pub reconstructed_crc: u32,
}

#[derive(Deserialize, Serialize)]
pub struct FactorioFileDescription {
	pub file_type: FactorioFileDescriptionType,
	pub file_name: String,
	pub content_size: u64,
	pub content_chunks: Vec<ChunkKey>,
}

#[derive(Deserialize, Serialize)]
pub enum FactorioFileDescriptionType {
	Normal,
	LevelDat {
		data_prefix: u16,
	}
}

pub enum FactorioFile<'a> {
	Normal(&'a [u8]),
	LevelDat {
		data_prefix: u16,
		data: Cow<'a, [u8]>,
	},
}

impl FactorioFile<'_> {
	pub fn data(&self) -> &[u8] {
		match self {
			FactorioFile::Normal(data) => data,
			FactorioFile::LevelDat { data, .. } => data,
		}
	}
}

pub fn deconstruct_world(world_data: &[u8], aux_data: &[u8]) -> anyhow::Result<(FactorioWorldDescription, HashMap<ChunkKey, Bytes>)> {
	let mut zip_reader = ZipArchive::new(Cursor::new(&world_data))?;
	
	let mut zip_writer = ZipWriter::new();
	let mut crc_hasher = FACTORIO_CRC.digest();
	let mut reconstructed_size: u32 = 0;
	
	let mut chunks = HashMap::new();
	let mut files = Vec::new();
	
	let mut buf = Vec::new();
	
	let mut add_data = |data: &[u8]| {
		crc_hasher.update(data);
		reconstructed_size += data.len() as u32;
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
	
	crc_hasher.update(aux_data);
	let reconstructed_crc = crc_hasher.finalize();
	
	let world = FactorioWorldDescription {
		files,
		aux_data: aux_data.to_vec().into(),
		reconstructed_size,
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
		
		let file = match file_desc.file_type {
			FactorioFileDescriptionType::Normal => FactorioFile::Normal(&buf),
			FactorioFileDescriptionType::LevelDat { data_prefix } => FactorioFile::LevelDat {
				data_prefix,
				data: Cow::Borrowed(&buf),
			}
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
		
		if self.current_size != world_desc.reconstructed_size as usize {
			return Err(anyhow::anyhow!("Client reconstructed size doesn't match server client: {}, server: {}",
				buf.len(), world_desc.reconstructed_size));
		}
		
		let world_block_length = (world_desc.reconstructed_size + TRANSFER_BLOCK_SIZE - 1) / TRANSFER_BLOCK_SIZE;
		let aux_block_length = (world_desc.aux_data.len() as u32 + TRANSFER_BLOCK_SIZE - 1) / TRANSFER_BLOCK_SIZE;
		
		let world_aligned_length = (world_block_length * TRANSFER_BLOCK_SIZE) as usize;
		let aux_aligned_length = (aux_block_length * TRANSFER_BLOCK_SIZE) as usize;
		
		buf.resize(world_aligned_length - self.current_size + buf.len(), 0);
		
		buf.put_slice(&world_desc.aux_data);
		buf.resize(aux_aligned_length - world_desc.aux_data.len() + buf.len(), 0);
		
		assert_eq!((old_size + buf.len()) % TRANSFER_BLOCK_SIZE as usize, 0);
		
		Ok(buf.freeze())
	}
}

pub fn decode_factorio_file<'a>(file_name: &str, mut file_data: &'a [u8]) -> anyhow::Result<FactorioFile<'a>> {
	let name = file_name.rsplit_once('/').map(|(_, last)| last).unwrap_or(file_name);
	
	if name.strip_prefix("level.dat").is_some_and(|suffix| suffix.chars().all(|c| c.is_ascii_digit())) {
		let data_prefix = file_data.try_get_u16_le()?;
		let uncompressed_data = miniz_oxide::inflate::decompress_to_vec_with_limit(file_data, 20_000_000)?;
		
		Ok(FactorioFile::LevelDat {
			data_prefix,
			data: uncompressed_data.into(),
		})
	} else {
		Ok(FactorioFile::Normal(file_data))
	}
}

pub fn encode_factorio_file<'a>(file: &'a FactorioFile<'a>) -> Cow<[u8]> {
	match file {
		FactorioFile::Normal(data) => Cow::Borrowed(data),
		FactorioFile::LevelDat { data_prefix, data } => {
			let mut buf = data_prefix.to_le_bytes().to_vec();
			buf.extend_from_slice(&miniz_oxide::deflate::compress_to_vec(data, RECONSTRUCT_DEFLATE_LEVEL));
			
			Cow::Owned(buf)
		},
	}
}

pub fn chunk_file(file_name: &str, file: &FactorioFile, chunks: &mut HashMap<ChunkKey, Bytes>) -> anyhow::Result<FactorioFileDescription> {
	let file_data = file.data();
	let chunker = Chunker::new(file_data);
	
	let file_type = match &file {
		FactorioFile::Normal(_) => FactorioFileDescriptionType::Normal,
		FactorioFile::LevelDat { data_prefix, .. } => FactorioFileDescriptionType::LevelDat {
			data_prefix: *data_prefix,
		},
	};
	
	let mut content_chunks = Vec::new();
	
	for chunk in chunker {
		let hash = ChunkKey(blake3::hash(chunk));
		
		content_chunks.push(hash);
		chunks.insert(hash, chunk.to_vec().into());
	}
	
	Ok(FactorioFileDescription {
		file_type,
		file_name: file_name.to_owned(),
		content_size: file_data.len() as u64,
		content_chunks,
	})
}

// pub fn reconstruct_file(file: &FactorioFileEntry, chunks: &HashMap<ChunkKey, Bytes>) -> anyhow::Result<Bytes> {
// 	let mut buf = BytesMut::new();
// 	
// 	
// 	
// 	Ok(buf.freeze())
// }

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