use bytes::{Buf, TryGetError};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

pub trait BufExt {
	fn try_get_factorio_varint32(&mut self) -> Result<u32, TryGetError>;
	fn try_get_factorio_varint16(&mut self) -> Result<u16, TryGetError>;
	fn try_advance(&mut self, len: usize) -> Result<(), TryGetError>;
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
	
	fn try_get_factorio_varint16(&mut self) -> Result<u16, TryGetError> {
		let byte = self.try_get_u8()?;
		
		if byte == 0xFF {
			self.try_get_u16_le()
		} else {
			Ok(byte as u16)
		}
	}
	
	fn try_advance(&mut self, len: usize) -> Result<(), TryGetError> {
		if self.remaining() < len {
			Err(TryGetError {
				requested: len,
				available: self.remaining(),
			})
		} else {
			self.advance(len);
			
			Ok(())
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

pub fn get_local_ip() -> anyhow::Result<IpAddr> {
	let sock = UdpSocket::bind(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0))?;
	sock.connect(SocketAddr::from(([10, 255, 255, 255], 1)))?;
	
	let sock_addr = sock.local_addr()?;
	
	Ok(sock_addr.ip())
}
