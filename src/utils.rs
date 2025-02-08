use bytes::{Buf, TryGetError};

pub trait BufExt {
	fn try_get_factorio_varint32(&mut self) -> Result<u32, TryGetError>;
}

impl<T: Buf> BufExt for T {
	fn try_get_factorio_varint32(&mut self) -> Result<u32, TryGetError> {
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