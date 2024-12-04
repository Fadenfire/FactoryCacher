use crate::chunker::Chunker;
use crate::factorio_protocol::{FACTORIO_CRC, FACTORIO_REV_CRC, TRANSFER_BLOCK_SIZE};
use crate::rev_crc;
use crate::zip_writer::ZipWriter;
use bytes::{BufMut, Bytes, BytesMut};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{Cursor, Read};
use zip::ZipArchive;

pub const RECONSTRUCT_DEFLATE_LEVEL: u8 = 1;

#[derive(Deserialize, Serialize)]
pub struct FactorioWorldDescription {
	pub files: Vec<FactorioFileDescription>,
	pub aux_data: Bytes,
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
	aux_data: &[u8],
) -> anyhow::Result<(FactorioWorldDescription, HashMap<ChunkKey, Bytes>)> {
	let mut zip_reader = ZipArchive::new(Cursor::new(&world_data))?;
	
	let mut chunks = HashMap::new();
	let mut files = Vec::new();
	
	let mut buf = Vec::new();
	
	for i in 0..zip_reader.len() {
		let mut zip_file = zip_reader.by_index(i)?;
		
		buf.clear();
		zip_file.read_to_end(&mut buf)?;
		
		let decoded_file = decode_factorio_file(zip_file.name(), &buf)?;
		
		files.push(chunk_file(zip_file.name(), &decoded_file, &mut chunks)?);
	}
	
	let world = FactorioWorldDescription {
		files,
		aux_data: aux_data.to_vec().into(),
	};
	
	Ok((world, chunks))
}

pub struct WorldReconstructor {
	zip_writer: ZipWriter,
	crc_hasher: crc::Digest<'static, u32>,
}

pub struct NeedsMoreData;

impl WorldReconstructor {
	pub fn new() -> Self {
		Self {
			zip_writer: ZipWriter::new(),
			crc_hasher: FACTORIO_CRC.digest(),
		}
	}
	
	pub fn reconstruct_world_file(
		&mut self,
		file_desc: &FactorioFileDescription,
		chunks: &HashMap<ChunkKey, Bytes>,
		buf: &mut BytesMut,
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
		
		self.crc_hasher.update(&header);
		self.crc_hasher.update(&file_data);
		
		Ok((header, file_data.into_owned().into()))
	}
	
	pub fn finalize_world_file(mut self,
		world_desc: &FactorioWorldDescription,
		target_world_size: usize,
		target_crc: u32,
	) -> anyhow::Result<Bytes> {
		let current_size = self.zip_writer.current_size();
		let zip_footer_size = self.zip_writer.central_directory_size();
		
		// The +4 is for the 4 bytes added in the middle to forge the CRC
		let total_size = current_size + zip_footer_size + 4;
		
		if total_size > target_world_size {
			return Err(anyhow::anyhow!("Reconstructed world size ({}) won't fit inside of estimated size ({})",
				total_size, target_world_size));
		}
		
		// Add padding to match target world size
		let mut output = BytesMut::zeroed(target_world_size - total_size);
		self.crc_hasher.update(&output);
		
		// Advance the zip writer offset to account for the padding
		// +4 for the CRC forge bytes
		self.zip_writer.advance_offset(output.len() + 4);
		
		// Encode central directory
		let zip_footer = self.zip_writer.encode_central_directory();
		
		// The CRC up to this point
		let forward_crc = self.crc_hasher.clone().finalize();
		
		// Reverse CRC from the end back to this point, starting with the target CRC
		let mut rev_crc_hasher = FACTORIO_REV_CRC.digest(target_crc);
		rev_crc_hasher.update(&world_desc.aux_data);
		rev_crc_hasher.update(&zip_footer);
		
		// Use the forward CRC and reverse CRC to generate 4 bytes that cause the overall CRC to be
		//  the target CRC
		let forge_bytes = rev_crc::forge_crc(forward_crc, rev_crc_hasher);
		
		// Append the forge bytes and central directory
		output.put_slice(&forge_bytes);
		output.put_slice(&zip_footer);
		
		// Verify that we padded the world size correctly
		assert_eq!(current_size + output.len(), target_world_size, "final world size is not equal to target world size");
		
		// Verify that we forged the CRC correctly
		
		self.crc_hasher.update(&forge_bytes);
		self.crc_hasher.update(&zip_footer);
		self.crc_hasher.update(&world_desc.aux_data);
		
		let final_crc = self.crc_hasher.finalize();
		assert_eq!(final_crc, target_crc, "Forging CRC failed");
		
		// Now align the world data to the nearest block
		
		let world_block_count = (target_world_size as u32 + TRANSFER_BLOCK_SIZE - 1) / TRANSFER_BLOCK_SIZE;
		let aux_block_count = (world_desc.aux_data.len() as u32 + TRANSFER_BLOCK_SIZE - 1) / TRANSFER_BLOCK_SIZE;
		
		let world_aligned_length = (world_block_count * TRANSFER_BLOCK_SIZE) as usize;
		let aux_aligned_length = (aux_block_count * TRANSFER_BLOCK_SIZE) as usize;
		
		output.resize(world_aligned_length - target_world_size + output.len(), 0);
		
		// Append the auxiliary data and align it to the nearest block
		output.put_slice(&world_desc.aux_data);
		output.resize(aux_aligned_length - world_desc.aux_data.len() + output.len(), 0);
		
		// Verify that the final data is properly aligned to the nearest block
		assert_eq!((current_size + output.len()) % TRANSFER_BLOCK_SIZE as usize, 0);
		
		Ok(output.freeze())
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
		}
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