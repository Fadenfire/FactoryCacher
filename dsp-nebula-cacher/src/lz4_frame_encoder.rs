use std::hash::Hasher;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use twox_hash::XxHash32;

const MAX_BLOCK_SIZE: usize = 64_000;

pub struct Lz4DeterministicFrameEncoder {
	pending_data: BytesMut,
	buffer: BytesMut,
	
	content_hasher: XxHash32,
}

impl Lz4DeterministicFrameEncoder {
	pub fn new() -> Self {
		let mut buffer = BytesMut::new();
		
		// Magic number
		buffer.put_u32_le(0x184D2204);
		// FLG byte: version - yes block independence - no block checksum - no content size - yes content checksum - reserved - no dict
		buffer.put_u8(0b01_1_0_0_1_0_0);
		// BD byte: reserved - block max size (4 means 64 KB) - reserved
		buffer.put_u8(0b0_100_0000);
		
		let header_checksum = XxHash32::oneshot(0, &buffer[4..]);
		buffer.put_u8(((header_checksum >> 8) & 0xFF) as u8);
		
		Self {
			pending_data: BytesMut::new(),
			buffer,
			
			content_hasher: XxHash32::with_seed(0),
		}
	}
	
	#[must_use]
	pub fn add_data(&mut self, data: &[u8]) -> Option<Bytes> {
		self.pending_data.put(data);
		
		if self.pending_data.len() >= MAX_BLOCK_SIZE {
			while self.pending_data.len() >= MAX_BLOCK_SIZE {
				self.build_block();
			}
			
			return Some(self.buffer.split().freeze());
		}
		
		None
	}
	
	#[must_use]
	pub fn finish(mut self) -> Bytes {
		if self.pending_data.has_remaining() {
			self.build_block();
		}
		
		self.buffer.put_u32_le(0); // End Mark
		self.buffer.put_u32_le(self.content_hasher.finish_32());
		
		self.buffer.freeze()
	}
	
	fn build_block(&mut self) {
		let block_size = MAX_BLOCK_SIZE.min(self.pending_data.len());
		let block_contents = self.pending_data.split_to(block_size);
		
		self.content_hasher.write(&block_contents);
		
		self.buffer.put_u32_le(0x80000000 | block_size as u32);
		self.buffer.put(block_contents);
	}
}

pub fn framed_size(original_len: u64) -> u64 {
	let mut final_len = 0;
	
	// Header
	final_len += 4; // Magic number
	final_len += 3; // FLG byte, BD byte, and header checksum byte
	
	// Blocks
	let total_blocks = (original_len + MAX_BLOCK_SIZE as u64 - 1) / MAX_BLOCK_SIZE as u64;
	
	final_len += total_blocks * 4;
	final_len += original_len;
	
	// Footer
	final_len += 4; // End Mark
	final_len += 4; // Content checksum
	
	final_len
}

#[cfg(test)]
mod tests {
	use std::io::Read;
	use std::iter;
	use super::*;
	
	#[test]
	fn test_lz4_framer() {
		let mut rng = fastrand::Rng::with_seed(99);
		
		let mut test_data: Vec<u8> = Vec::new();
		let mut lz4_data: Vec<u8> = Vec::new();
		
		let mut frame_encoder = Lz4DeterministicFrameEncoder::new();
		
		for _ in 0..432 {
			let chunk_size = rng.usize(10_000..50_000);
			let test_chunk: Vec<u8> = iter::repeat_with(|| rng.u8(..)).take(chunk_size).collect();
			
			test_data.extend_from_slice(&test_chunk);
			
			if let Some(bytes) = frame_encoder.add_data(&test_chunk) {
				lz4_data.extend_from_slice(&bytes);
			}
		}
		
		lz4_data.extend_from_slice(&frame_encoder.finish());
		
		assert_eq!(framed_size(test_data.len() as u64) as usize, lz4_data.len());
		
		let mut frame_decoder = lz4_flex::frame::FrameDecoder::new(lz4_data.as_slice());
		
		let mut decoded_data = Vec::new();
		frame_decoder.read_to_end(&mut decoded_data).unwrap();
		
		assert_eq!(test_data, decoded_data);
	}
}