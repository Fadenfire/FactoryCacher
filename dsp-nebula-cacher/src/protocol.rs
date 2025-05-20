use bytes::{Buf, BufMut};
use fastwebsockets::OpCode;
use quinn_proto::coding::Codec;
use quinn_proto::VarInt;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct InitialConnectionInfo {
	pub client_uri: String,
}

#[derive(Debug)]
pub enum WsFrameHeader {
	DedupedPacket {
		stream_id: u64,
	},
	NormalFrame {
		op_code: OpCode,
		fin: bool,
		data_size: VarInt,
	}
}

impl WsFrameHeader {
	pub fn encode(&self, mut buf: &mut impl BufMut) {
		match *self {
			WsFrameHeader::DedupedPacket { stream_id } => {
				buf.put_u8(1 << 7);
				buf.put_u64(stream_id);
			}
			WsFrameHeader::NormalFrame { op_code, fin, data_size } => {
				let mut byte = op_code as u8;
				if byte > 0xF { unreachable!("Opcode out of range"); }
				
				if fin { byte |= 1 << 4; }
				
				buf.put_u8(byte);
				data_size.encode(&mut buf);
			}
		}
	}
	
	pub fn decode(buf: &mut impl Buf) -> anyhow::Result<Self> {
		let byte = buf.try_get_u8()?;
		
		if byte & (1 << 7) != 0 {
			let stream_id = buf.try_get_u64()?;
			
			Ok(Self::DedupedPacket {
				stream_id,
			})
		} else {
			let data_size = VarInt::decode(buf)?;
			let op_code = OpCode::try_from(byte & 0xF)?;
			
			Ok(Self::NormalFrame {
				op_code,
				fin: byte & (1 << 4) != 0,
				data_size,
			})
		}
	}
}