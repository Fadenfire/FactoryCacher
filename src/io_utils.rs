use bytes::Buf;

#[derive(Debug)]
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