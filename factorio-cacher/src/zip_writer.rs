use std::borrow::Cow;
use bytes::{BufMut, Bytes, BytesMut};
use crc::Crc;

pub const ZIP_CRC: Crc<u32> = Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);

pub struct ZipWriter {
	current_offset: usize,
	entries: Vec<ZipEntryMetadata>,
}

struct ZipEntryMetadata {
	start_offset: usize,
	file_name: String,
	file_name_size: u16,
	compressed_size: u32,
	uncompressed_size: u32,
	compression_method: u16,
	data_crc: u32,
}

impl ZipWriter {
	pub const CENTRAL_DIRECTORY_ENTRY_SIZE: usize = 46;
	pub const END_OF_CENTRAL_DIRECTORY_SIZE: usize = 22;
	
	pub fn new() -> Self {
		Self {
			current_offset: 0,
			entries: Vec::new(),
		}
	}
	
	pub fn encode_file(&mut self, file_name: &str, file_data: &[u8], deflate_level: Option<u8>) -> Bytes {
		let mut buf = BytesMut::new();
		
		let mut final_data = Cow::Borrowed(file_data);
		let mut compression_method = 0; // 0 = none, 8 = deflate
		
		if let Some(deflate_level) = deflate_level {
			final_data = Cow::Owned(miniz_oxide::deflate::compress_to_vec(file_data, deflate_level));
			compression_method = 8;
		}
		
		let data_crc = ZIP_CRC.checksum(file_data);
		let uncompressed_size: u32 = file_data.len().try_into().expect("Zip entry uncompressed size didn't fit in u32");
		let compressed_size: u32 = final_data.len().try_into().expect("Zip entry compressed size didn't fit in u32");
		let file_name_size: u16 = file_name.len().try_into().expect("File name length didn't fit in u16");
		
		buf.put_u32_le(0x04034b50); // Magic number
		buf.put_u16_le(45); // Version needed to extract
		buf.put_u16_le(0); // Flags
		buf.put_u16_le(compression_method); // Compression method, 0 = none, 8 = deflate
		buf.put_u16_le(0); // File last modification time
		buf.put_u16_le(0); // File last modification date
		buf.put_u32_le(data_crc); // File crc
		buf.put_u32_le(compressed_size); // Compressed size
		buf.put_u32_le(uncompressed_size); // Uncompressed size
		buf.put_u16_le(file_name_size); // File name length
		buf.put_u16_le(0); // Extra field length
		buf.extend_from_slice(file_name.as_bytes()); // File name
		
		self.entries.push(ZipEntryMetadata {
			start_offset: self.current_offset,
			file_name: file_name.to_owned(),
			file_name_size,
			compressed_size,
			uncompressed_size,
			compression_method,
			data_crc,
		});
		
		self.current_offset += buf.len();
		self.current_offset += final_data.len();
		
		buf.extend_from_slice(&final_data);
		buf.freeze()
	}
	
	pub fn advance_offset(&mut self, offset: usize) {
		self.current_offset += offset;
	}
	
	pub fn encode_central_directory(&self) -> Bytes {
		let mut buf = BytesMut::new();
		
		let central_directory_offset: u32 = self.current_offset.try_into().expect("Central directory offset didn't fit in u32");
		let entry_count: u16 = self.entries.len().try_into().expect("Entry count did not fit in u16");
		
		for entry in &self.entries {
			let offset: u32 = entry.start_offset.try_into().expect("Entry offset didn't fit in u32");
			
			buf.put_u32_le(0x02014b50); // Magic number
			buf.put_u16_le(45); // Version made by
			buf.put_u16_le(45); // Version needed to extract
			buf.put_u16_le(0); // Flags
			buf.put_u16_le(entry.compression_method); // Compression method, 0 = none, 8 = deflate
			buf.put_u16_le(0); // File last modification time
			buf.put_u16_le(0); // File last modification date
			buf.put_u32_le(entry.data_crc); // File crc
			buf.put_u32_le(entry.compressed_size); // Compressed size
			buf.put_u32_le(entry.uncompressed_size); // Uncompressed size
			buf.put_u16_le(entry.file_name_size); // File name length
			buf.put_u16_le(0); // Extra field length
			buf.put_u16_le(0); // File comment length
			buf.put_u16_le(0); // Disk number where file starts
			buf.put_u16_le(0); // Internal file attributes
			buf.put_u32_le(0); // External file attributes
			buf.put_u32_le(offset);
			buf.extend_from_slice(entry.file_name.as_bytes()); // File name
		}
		
		let central_directory_size: u32 = buf.len().try_into().expect("Central directory size didn't fit in u32");
		
		buf.put_u32_le(0x06054b50); // Magic number
		buf.put_u16_le(0); // Number of this disk
		buf.put_u16_le(0); // Disk where central directory starts
		buf.put_u16_le(entry_count); // Number of central directory records on this disk
		buf.put_u16_le(entry_count); // Total number of central directory records
		buf.put_u32_le(central_directory_size); // Size of central directory
		buf.put_u32_le(central_directory_offset); // Offset of start of central directory
		buf.put_u16_le(0); // Comment length
		
		buf.freeze()
	}
	
	pub fn central_directory_size(&self) -> usize {
		let mut size = 0;
		
		for entry in &self.entries {
			size += Self::CENTRAL_DIRECTORY_ENTRY_SIZE + entry.file_name.len();
		}
		
		size + Self::END_OF_CENTRAL_DIRECTORY_SIZE
	}
	
	pub fn current_size(&self) -> usize {
		self.current_offset
	}
}

#[cfg(test)]
mod test {
	use std::io::Read;
	use zip::CompressionMethod;
	use super::*;
	
	const TEST_FILE1_NAME: &str = "test.txt";
	const TEST_FILE1_CONTENTS: &[u8] = b"This is a test";
	
	const TEST_FILE2_NAME: &str = "compressed.txt";
	const TEST_FILE2_CONTENTS: &[u8] = b"This is also a test, but compressed";
	
	#[test]
	fn test_zip_writer() {
		let mut zip_writer = ZipWriter::new();
		let mut zip_data = Vec::new();
		
		zip_data.extend_from_slice(&zip_writer.encode_file(TEST_FILE1_NAME, TEST_FILE1_CONTENTS, None));
		zip_data.extend_from_slice(&zip_writer.encode_file(TEST_FILE2_NAME, TEST_FILE2_CONTENTS, Some(1)));
		zip_data.extend_from_slice(&zip_writer.encode_central_directory());
		
		let mut zip_reader = zip::ZipArchive::new(std::io::Cursor::new(zip_data)).unwrap();
		
		for i in 0..zip_reader.len() {
			let mut zip_file = zip_reader.by_index(i).unwrap();
			
			let mut buf = Vec::new();
			zip_file.read_to_end(&mut buf).unwrap();
			
			match zip_file.name() {
				TEST_FILE1_NAME => {
					assert_eq!(buf, TEST_FILE1_CONTENTS);
					assert_eq!(zip_file.compression(), CompressionMethod::Stored);
				},
				TEST_FILE2_NAME => {
					assert_eq!(buf, TEST_FILE2_CONTENTS);
					assert_eq!(zip_file.compression(), CompressionMethod::Deflated);
				},
				name => {
					panic!("Unexpected zip file name: {}", name);
				}
			}
		}
	}
}