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
	data_size: u32,
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
	
	pub fn encode_file_header(&mut self, file_name: &str, file_data: &[u8]) -> Bytes {
		let mut buf = BytesMut::new();
		
		let data_crc = ZIP_CRC.checksum(file_data);
		let data_size: u32 = file_data.len().try_into().expect("Zip entry size didn't fit in u32");
		let file_name_size: u16 = file_name.len().try_into().expect("File name length didn't fit in u16");
		
		buf.put_u32_le(0x04034b50); // Magic number
		buf.put_u16_le(45); // Version needed to extract
		buf.put_u16_le(0); // Flags
		buf.put_u16_le(0); // Compression method, 0 = none
		buf.put_u16_le(0); // File last modification time
		buf.put_u16_le(0); // File last modification date
		buf.put_u32_le(data_crc); // File crc
		buf.put_u32_le(data_size); // Compressed size
		buf.put_u32_le(data_size); // Uncompressed size
		buf.put_u16_le(file_name_size); // File name length
		buf.put_u16_le(0); // Extra field length
		buf.extend_from_slice(file_name.as_bytes()); // File name
		
		self.entries.push(ZipEntryMetadata {
			start_offset: self.current_offset,
			file_name: file_name.to_owned(),
			file_name_size,
			data_size,
			data_crc,
		});
		
		self.current_offset += buf.len();
		self.current_offset += file_data.len();
		
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
			buf.put_u16_le(0); // Compression method, 0 = none
			buf.put_u16_le(0); // File last modification time
			buf.put_u16_le(0); // File last modification date
			buf.put_u32_le(entry.data_crc); // File crc
			buf.put_u32_le(entry.data_size); // Compressed size
			buf.put_u32_le(entry.data_size); // Uncompressed size
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