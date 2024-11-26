use std::collections::BTreeSet;
use bytes::{Bytes, BytesMut};
use std::net::SocketAddr;
use tokio::net::UdpSocket;
use crate::packet_parser::{parse_packet, MapReadyForDownloadData, PacketType, ServerToClientHeartbeatPacket, TransferBlockPacket, TransferBlockRequestPacket};

const FACTORIO_CRC: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);

#[tokio::main]
pub async fn proxy_test() {
	let server_addr = SocketAddr::from(([127, 0, 0, 1], 60135));
	let proxy_addr = SocketAddr::from(([127, 0, 0, 1], 7000));
	let mut client_addr: Option<SocketAddr> = None;
	
	let sock = UdpSocket::bind(proxy_addr).await.unwrap();
	let mut buf = BytesMut::new();
	let mut out_packets = Vec::new();
	
	let mut state = ProxyState::new();
	
	println!("Started");
	
	loop {
		buf.clear();
		buf.reserve(8192);
		let remote_addr = sock.recv_buf_from(&mut buf).await.unwrap().1;
		
		if client_addr.is_none() && remote_addr != server_addr {
			client_addr = Some(remote_addr);
			println!("Got client addr: {:?}", client_addr);
		}
		
		let Some(client_addr) = client_addr else { continue; };
		
		let data = buf.split().freeze();
		
		if remote_addr == server_addr {
			state.recv_packet_from_server(data, &mut out_packets);
		} else if remote_addr == client_addr {
			state.recv_packet_from_client(data, &mut out_packets);
		}
		
		for (out_data, dir) in out_packets.drain(..) {
			let dest_addr = match dir {
				Direction::Client => client_addr,
				Direction::Server => server_addr,
			};
			
			sock.send_to(&out_data, dest_addr).await.unwrap();
		}
	}
}

enum Direction {
	Client,
	Server,
}

struct ProxyState {
	phase: ProxyStatePhase,
}

enum ProxyStatePhase {
	WaitingForWorld,
	DownloadingWorld {
		held_packets: Vec<Bytes>,
		world_info: MapReadyForDownloadData,
		world_chunks: Vec<TransferBlockPacket>,
		chunk_request_queue: BTreeSet<u32>,
	},
	SendingWorld {
		world_data: Bytes,
	},
}

impl ProxyState {
	pub fn new() -> Self {
		Self {
			phase: ProxyStatePhase::WaitingForWorld,
		}
	}
	
	pub fn recv_packet_from_server(&mut self, packet_data: Bytes, out_packets: &mut Vec<(Bytes, Direction)>) {
		let packet = parse_packet(packet_data.clone()).unwrap();
		
		match &mut self.phase {
			ProxyStatePhase::WaitingForWorld => {
				if packet.packet_type == PacketType::ServerToClientHeartbeat {
					let heartbeat = ServerToClientHeartbeatPacket::decode(packet.data).unwrap();
					
					if let Some((world_info, _)) = &heartbeat.map_ready_to_download_data {
						println!("Got world info: {:?}", world_info);
						
						let total_chunks = (world_info.world_size + 502) / 503 + (world_info.aux_size + 502) / 503;
						
						println!("Transitioning to DownloadingWorld");
						self.phase = ProxyStatePhase::DownloadingWorld {
							held_packets: vec![packet_data],
							world_info: world_info.clone(),
							world_chunks: Vec::new(),
							chunk_request_queue: BTreeSet::from_iter(0..total_chunks),
						};
						
						let first_request = TransferBlockRequestPacket {
							block_id: 0,
						};
						
						out_packets.push((first_request.as_factorio_packet().encode(), Direction::Server));
						
						return;
					}
				}
			}
			ProxyStatePhase::DownloadingWorld {
				held_packets,
				world_info,
				world_chunks,
				chunk_request_queue
			} => {
				if packet.packet_type == PacketType::TransferBlock {
					let transfer_block = TransferBlockPacket::decode(packet.data).unwrap();
					
					if chunk_request_queue.remove(&transfer_block.block_id) {
						// println!("Got block from server: {:?}", transfer_block.block_id);
						world_chunks.push(transfer_block);
					}
					
					if chunk_request_queue.is_empty() {
						println!("Got final block");
						
						world_chunks.sort_by_key(|block| block.block_id);
						
						let mut world_data = Vec::new();
						
						for block in world_chunks {
							world_data.extend_from_slice(&block.data);
						}
						
						println!("Writing world out");
						std::fs::write("joe.zip", &world_data[..world_info.world_size as usize]).unwrap();
						
						let new_world_size = world_info.world_size + 1;
						let aux_offset = ((world_info.world_size + 502) / 503 * 503) as usize;
						
						let mut crc_hasher = FACTORIO_CRC.digest();
						crc_hasher.update(&world_data[..new_world_size as usize]);
						crc_hasher.update(&world_data[aux_offset..aux_offset + world_info.aux_size as usize]);
						
						let new_crc = crc_hasher.finalize();
						
						for mut held_packet_data in held_packets.drain(..) {
							let held_packet = parse_packet(held_packet_data.clone()).unwrap();
							
							if held_packet.packet_type == PacketType::ServerToClientHeartbeat {
								let mut heartbeat = ServerToClientHeartbeatPacket::decode(held_packet.data).unwrap();
								
								if let Some((pak_world_info, _)) = &mut heartbeat.map_ready_to_download_data {
									pak_world_info.world_size = new_world_size;
									pak_world_info.world_crc = new_crc;
									
									println!("Updating map ready packet with new world info");
									held_packet_data = heartbeat.as_factorio_packet().encode();
								}
							}
							
							out_packets.push((held_packet_data, Direction::Client));
						}
						
						println!("Transitioning to SendingWorld");
						self.phase = ProxyStatePhase::SendingWorld {
							world_data: world_data.into(),
						};
					} else {
						// println!("{} blocks left", chunk_request_queue.len());
						
						let next_block_id = *chunk_request_queue.first().unwrap();
						
						let request = TransferBlockRequestPacket {
							block_id: next_block_id,
						};
						
						out_packets.push((request.as_factorio_packet().encode(), Direction::Server));
					}
				} else {
					held_packets.push(packet_data);
				}
				
				return;
			}
			_ => {}
		}
		
		out_packets.push((packet_data, Direction::Client));
	}
	
	pub fn recv_packet_from_client(&mut self, packet_data: Bytes, out_packets: &mut Vec<(Bytes, Direction)>) {
		let packet = parse_packet(packet_data.clone()).unwrap();
		
		match &mut self.phase {
			ProxyStatePhase::WaitingForWorld => {}
			ProxyStatePhase::DownloadingWorld { .. } => {}
			ProxyStatePhase::SendingWorld { world_data } => {
				if packet.packet_type == PacketType::TransferBlockRequest {
					let request = TransferBlockRequestPacket::decode(packet.data).unwrap();
					let offset = request.block_id as usize * 503;
					
					println!("Client requested {}", request.block_id);
					
					if offset + 503 <= world_data.len() {
						let block = TransferBlockPacket {
							block_id: request.block_id,
							data: world_data.slice(offset..offset + 503),
						};
						
						println!("Sending {}", request.block_id);
						
						out_packets.push((block.as_factorio_packet().encode(), Direction::Client));
					}
					
					return;
				}
			}
		}
		
		out_packets.push((packet_data, Direction::Server));
	}
}