#[derive(Debug, Clone)]
pub struct RabinKarpHash {
	hash: u32,
	window: [u32; Self::WINDOW_SIZE],
	window_pos: usize,
}

impl RabinKarpHash {
	const OFFSET: u32 = 31;
	const MULT: u32 = 0x08104225;
	const WINDOW_MULT: u32 = u32::wrapping_pow(Self::MULT, (Self::WINDOW_SIZE as u32) + 1);
	
	pub const WINDOW_SIZE: usize = 32;
	
	pub fn new() -> Self {
		Self {
			hash: 0,
			window: [0; Self::WINDOW_SIZE],
			window_pos: 0,
		}
	}
	
	#[inline]
	pub fn update(&mut self, added_byte: u8) -> u32 {
		unsafe {
			let added = (added_byte as u32).wrapping_add(Self::OFFSET);
			let removed = self.window.get_unchecked(self.window_pos).wrapping_mul(Self::WINDOW_MULT);
			
			self.hash = self.hash.wrapping_add(added).wrapping_mul(Self::MULT).wrapping_sub(removed);
			
			*self.window.get_unchecked_mut(self.window_pos) = added;
			self.window_pos = (self.window_pos + 1) % Self::WINDOW_SIZE;
			
			self.hash
		}
	}
	
	#[inline]
	pub fn digest(&self) -> u32 {
		self.hash
	}
	
	pub fn reset(&mut self) {
		self.hash = 0;
		self.window.fill(0);
		self.window_pos = 0;
	}
}

const MIN_CHUNK_SIZE: usize = 1 << 9;
const MAX_CHUNK_SIZE: usize = 1 << 12;
const CHUNK_MASK: u32 = (1 << 10) - 1;

pub struct Chunker<'a> {
	rolling_hash: RabinKarpHash,
	data: &'a [u8],
}

impl<'a> Chunker<'a> {
	pub fn new(data: &'a [u8]) -> Self {
		Self {
			rolling_hash: RabinKarpHash::new(),
			data,
		}
	}
}

impl<'a> Iterator for Chunker<'a> {
	type Item = &'a [u8];
	
	fn next(&mut self) -> Option<Self::Item> {
		if self.data.is_empty() {
			return None;
		}
		
		let mut chunk_size = MIN_CHUNK_SIZE.min(self.data.len());
		
		for &byte in &self.data[chunk_size..] {
			let hash = self.rolling_hash.update(byte);
			
			chunk_size += 1;
			
			if (hash & CHUNK_MASK) == 0 || chunk_size >= MAX_CHUNK_SIZE {
				break;
			}
		}
		
		let chunk = &self.data[..chunk_size];
		
		self.data = &self.data[chunk_size..];
		self.rolling_hash.reset();
		
		Some(chunk)
	}
}
