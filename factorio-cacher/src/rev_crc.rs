use crc::{Algorithm, Crc, Table};

pub struct RevCRC {
	algorithm: &'static Algorithm<u32>,
	reverse_table: [u32; 256],
}

impl RevCRC {
	pub const fn new(crc: &Crc<u32, Table<1>>) -> Self {
		let table = &crc.table()[0];
		let mut reverse_table = [0; 256];
		
		let mut n = 0;
		
		while n < table.len() {
			reverse_table[(table[n] >> 24) as usize] = (table[n] << 8) ^ n as u32;
			n += 1;
		}
		
		Self {
			algorithm: crc.algorithm,
			reverse_table,
		}
	}
	
	fn update(&self, mut value: u32, data: &[u8]) -> u32 {
		for &byte in data.iter().rev() {
			value = (value << 8) ^ self.reverse_table[(value >> 24) as usize] ^ byte as u32;
		}
		
		value
	}
	
	pub fn digest(&self, initial_value: u32) -> RevDigest<'_> {
		RevDigest {
			crc: self,
			value: initial_value ^ self.algorithm.xorout,
		}
	}
}

#[derive(Clone)]
pub struct RevDigest<'a> {
	crc: &'a RevCRC,
	value: u32,
}

impl<'a> RevDigest<'a> {
	/// Data must be appended in reverse order
	pub fn update(&mut self, data: &[u8]) {
		self.value = self.crc.update(self.value, data);
	}
	
	pub fn finalize(self) -> u32 {
		self.value
	}
}

/// Takes in a before digest and an after digest and computes what 4 fours mus be placed
///  in the middle to make the overall CRC be equal to the initial value of after_digest
pub fn forge_crc(mut before_digest: u32, mut after_digest: RevDigest) -> [u8; 4] {
	before_digest ^= after_digest.crc.algorithm.xorout;
	
	after_digest.update(&before_digest.to_le_bytes());
	after_digest.finalize().to_le_bytes()
}
