#![allow(dead_code)]

use bytes::Buf;
use thiserror::Error;

#[derive(Debug, Error)]
#[error("unexpected end of file")]
pub struct UnexpectedEOF;

pub trait BufExt {
	fn try_advance(&mut self, n: usize) -> Result<(), UnexpectedEOF>;
	fn try_get_u8(&mut self) -> Result<u8, UnexpectedEOF>;
	fn try_get_u16_le(&mut self) -> Result<u16, UnexpectedEOF>;
	fn try_get_u32_le(&mut self) -> Result<u32, UnexpectedEOF>;
	fn try_get_i32_le(&mut self) -> Result<i32, UnexpectedEOF>;
	fn try_get_factorio_varint32(&mut self) -> Result<u32, UnexpectedEOF>;
}

impl<T: Buf> BufExt for T {
	fn try_advance(&mut self, n: usize) -> Result<(), UnexpectedEOF> {
		if self.remaining() < n { return Err(UnexpectedEOF); }
		
		self.advance(n);
		Ok(())
	}
	
	fn try_get_u8(&mut self) -> Result<u8, UnexpectedEOF> {
		if self.remaining() < 1 { return Err(UnexpectedEOF); }
		
		Ok(self.get_u8())
	}
	
	fn try_get_u16_le(&mut self) -> Result<u16, UnexpectedEOF> {
		if self.remaining() < 2 { return Err(UnexpectedEOF); }
		
		Ok(self.get_u16_le())
	}
	
	fn try_get_u32_le(&mut self) -> Result<u32, UnexpectedEOF> {
		if self.remaining() < 4 { return Err(UnexpectedEOF); }
		
		Ok(self.get_u32_le())
	}
	
	fn try_get_i32_le(&mut self) -> Result<i32, UnexpectedEOF> {
		if self.remaining() < 4 { return Err(UnexpectedEOF); }
		
		Ok(self.get_i32_le())
	}
	
	fn try_get_factorio_varint32(&mut self) -> Result<u32, UnexpectedEOF> {
		let byte = self.try_get_u8()?;
		
		if byte == 0xFF {
			self.try_get_u32_le()
		} else {
			Ok(byte as u32)
		}
	}
}

const POWER_UNITS: &[char] = &['k', 'M', 'G', 'T', 'P', 'E', 'Z', 'Y'];

pub fn abbreviate_number(num: u64) -> String {
	if num <= 0 { return num.to_string(); }
	
	let power = num.ilog(1000);
	if power <= 0 { return num.to_string(); }
	
	let x = num as f64 / 1000u64.pow(power) as f64;
	let unit = POWER_UNITS.get((power - 1) as usize).unwrap_or(&'?');
	
	format!("{:.2}{}", x, unit)
}