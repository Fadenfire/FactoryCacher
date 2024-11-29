use crate::dedup::{ChunkKey, FactorioWorldDescription};
use crate::factorio_protocol::{FactorioPacket, FactorioPacketHeader, MapReadyForDownloadData, PacketType, ServerToClientHeartbeatPacket, TransferBlockPacket, TransferBlockRequestPacket, TRANSFER_BLOCK_SIZE};
use crate::protocol::{Datagram, RequestChunksMessage, SendChunksMessage, WorldReadyMessage};
use crate::proxy::PacketDirection;
use crate::{dedup, protocol};
use bytes::{Bytes, BytesMut};
use log::{error, info};
use quinn_proto::VarInt;
use socket2::SockRef;
use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::UdpSocket;
use tokio::select;
use tokio::sync::mpsc;
use tokio::time::Instant;

pub async fn run_server_proxy(
	connection: Arc<quinn::Connection>,
	factorio_addr: SocketAddr,
) -> anyhow::Result<()> {
	let mut outgoing_queues: HashMap<VarInt, mpsc::Sender<Bytes>> = HashMap::new();
	
	loop {
		select! {
            result = connection.read_datagram() => {
                let datagram = Datagram::decode(result?)?;

                if let Some(outgoing_queue) = outgoing_queues.get(&datagram.peer_id) {
                    let _ = outgoing_queue.try_send(datagram.data);
                }
            }
            result = connection.accept_bi() => {
                let (send_stream, mut recv_stream) = result?;
                let peer_id: VarInt = recv_stream.read_u32_le().await?.into();

                let localhost: IpAddr = if factorio_addr.is_ipv6() {
                    Ipv6Addr::LOCALHOST.into()
                } else {
                    Ipv4Addr::LOCALHOST.into()
                };

                let socket = UdpSocket::bind((localhost, 0)).await?;
				SockRef::from(&socket).set_recv_buffer_size(16 * 1024 * 1024)?;
				
                let (receive_queue_tx, receive_queue_rx) = mpsc::channel(8192);

                tokio::spawn(proxy_server(ProxyServerArgs {
                    connection: connection.clone(),
                    peer_id,

                    socket,
                    factorio_addr,

                    receive_queue_rx,

                    comp_stream: (send_stream, recv_stream),
                }));

                outgoing_queues.insert(peer_id, receive_queue_tx);
            }
        }
	}
}

struct ProxyServerArgs {
	connection: Arc<quinn::Connection>,
	peer_id: VarInt,
	
	socket: UdpSocket,
	factorio_addr: SocketAddr,
	
	receive_queue_rx: mpsc::Receiver<Bytes>,
	
	comp_stream: (quinn::SendStream, quinn::RecvStream),
}

async fn proxy_server(mut args: ProxyServerArgs) {
	let mut buf = BytesMut::new();
	let mut out_packets = Vec::new();
	
	let mut proxy_state = ServerProxyState::new(args.comp_stream);
	
	loop {
		buf.clear();
		buf.reserve(8192);
		
		select! {
            result = args.socket.recv_buf_from(&mut buf) => {
                let Ok((_, remote_addr)) = result else { return };

                // Drop any packets that don't originate from the server
                if remote_addr != args.factorio_addr { continue; }

                proxy_state.on_packet_from_server(buf.split().freeze(), &mut out_packets);
				// out_packets.push((buf.split().freeze(), PacketDirection::ToClient));
            }
            result = args.receive_queue_rx.recv() => {
                let Some(packet_data) = result else { return; };

                out_packets.push((packet_data, PacketDirection::ToServer));
            }
            // _ = tokio::time::sleep(UDP_PEER_IDLE_TIMEOUT) => return
        }
		
		for (packet_data, dir) in out_packets.drain(..) {
			match dir {
				PacketDirection::ToClient => {
					Datagram::new(args.peer_id, packet_data).encode(&mut buf);
					
					if args.connection.send_datagram(buf.split().freeze()).is_err() {
						return;
					}
				}
				PacketDirection::ToServer => {
					if args.socket.send_to(&packet_data, args.factorio_addr).await.is_err() {
						return;
					}
				}
			}
		}
	}
}

struct ServerProxyState {
	phase: ServerProxyPhase,
	comp_stream: Option<(quinn::SendStream, quinn::RecvStream)>,
}

enum ServerProxyPhase {
	WaitingForWorld,
	DownloadingWorld(DownloadingWorldState),
	Done,
}

struct DownloadingWorldState {
	held_packets: Vec<Bytes>,
	world_info: MapReadyForDownloadData,
	world_block_count: u32,
	received_blocks: Vec<TransferBlockPacket>,
	block_request_queue: BTreeSet<u32>,
	last_block_time: Instant,
}

impl ServerProxyState {
	pub fn new(comp_stream: (quinn::SendStream, quinn::RecvStream)) -> Self {
		Self {
			phase: ServerProxyPhase::WaitingForWorld,
			comp_stream: Some(comp_stream),
		}
	}
	
	pub fn on_packet_from_server(
		&mut self,
		in_packet_data: Bytes,
		out_packets: &mut Vec<(Bytes, PacketDirection)>,
	) {
		match &mut self.phase {
			ServerProxyPhase::WaitingForWorld => {
				if let Ok((header, msg_data)) =
					FactorioPacketHeader::decode(in_packet_data.clone())
				{
					if header.packet_type == PacketType::ServerToClientHeartbeat {
						let result = ServerToClientHeartbeatPacket::decode(msg_data)
							.and_then(ServerToClientHeartbeatPacket::try_decode_map_ready);
						
						if let Ok(Some(world_info)) = result {
							self.transition_to_downloading_world(in_packet_data, world_info, out_packets);
							return;
						}
					}
				}
			}
			ServerProxyPhase::DownloadingWorld(state) => {
				if let Ok((header, msg_data)) =
					FactorioPacketHeader::decode(in_packet_data.clone())
				{
					if header.packet_type == PacketType::TransferBlock {
						let Ok(transfer_block) = TransferBlockPacket::decode(msg_data)
						else { return; };
						
						if state.block_request_queue.remove(&transfer_block.block_id) {
							state.received_blocks.push(transfer_block);
							
							state.last_block_time = Instant::now();
						}
						
						if state.block_request_queue.is_empty() {
							self.finalize_world(out_packets);
							return;
						} else {
							let next_block_id = *state.block_request_queue.first().unwrap();
							let request = TransferBlockRequestPacket { block_id: next_block_id };
							
							out_packets.push((request.encode_full_packet(), PacketDirection::ToServer));
						}
					} else {
						state.held_packets.push(in_packet_data);
					}
				}
				
				if (Instant::now() - state.last_block_time) > Duration::from_millis(100) {
					let next_block_id = *state.block_request_queue.first().unwrap();
					let request = TransferBlockRequestPacket { block_id: next_block_id };
					
					out_packets.push((request.encode_full_packet(), PacketDirection::ToServer));
				}
				
				return;
			}
			ServerProxyPhase::Done => {}
		}
		
		out_packets.push((in_packet_data, PacketDirection::ToClient));
	}
	
	fn transition_to_downloading_world(
		&mut self,
		in_packet_data: Bytes,
		world_info: MapReadyForDownloadData,
		out_packets: &mut Vec<(Bytes, PacketDirection)>,
	) {
		info!("Got world info: {:?}", world_info);
		
		let world_block_count = (world_info.world_size + TRANSFER_BLOCK_SIZE - 1) / TRANSFER_BLOCK_SIZE;
		let aux_block_count = (world_info.aux_size + TRANSFER_BLOCK_SIZE - 1) / TRANSFER_BLOCK_SIZE;
		
		let total_block_count = world_block_count + aux_block_count;
		
		let state = DownloadingWorldState {
			held_packets: vec![in_packet_data],
			world_info,
			world_block_count,
			received_blocks: Vec::new(),
			block_request_queue: BTreeSet::from_iter(0..total_block_count),
			last_block_time: Instant::now(),
		};
		
		self.phase = ServerProxyPhase::DownloadingWorld(state);
		
		let first_request = TransferBlockRequestPacket { block_id: 0 };
		out_packets.push((first_request.encode_full_packet(), PacketDirection::ToServer));
	}
	
	fn finalize_world(&mut self, out_packets: &mut Vec<(Bytes, PacketDirection)>) {
		let state = match &mut self.phase {
			ServerProxyPhase::DownloadingWorld(state) => state,
			_ => unreachable!(),
		};
		
		info!("Got last block");
		
		state.received_blocks.sort_by_key(|block| block.block_id);
		
		let mut received_data = Vec::new();
		
		for block in state.received_blocks.drain(..) {
			received_data.extend_from_slice(&block.data);
		}
		
		let aux_data_offset = state.world_block_count * TRANSFER_BLOCK_SIZE;
		
		let world_data = &received_data[..state.world_info.world_size as usize];
		let aux_data = &received_data[aux_data_offset as usize..(aux_data_offset + state.world_info.aux_size) as usize];
		
		let (world_description, chunks) =
			match dedup::deconstruct_world(world_data, aux_data) {
				Ok(result) => result,
				Err(err) => {
					error!("Error trying to deconstruct world: {:?}", err);
					
					self.phase = ServerProxyPhase::Done;
					return;
				}
			};
		
		let new_world_info = MapReadyForDownloadData {
			world_size: world_description.world_size,
			world_crc: world_description.reconstructed_crc,
			..state.world_info
		};
		
		info!("Calc new world info: {:?}", new_world_info);
		
		let comp_stream = self.comp_stream.take().unwrap();
		
		tokio::spawn(async move {
			if let Err(err) = transfer_world_data(comp_stream.0, comp_stream.1, world_description, chunks).await {
				error!("Error trying to transfer world data: {:?}", err);
			}
		});
		
		let mut old_world_info_encoded = Vec::new();
		let mut new_world_info_encoded = Vec::new();
		
		state.world_info.encode(&mut old_world_info_encoded);
		new_world_info.encode(&mut new_world_info_encoded);
		
		// TODO: Apply this replacement to all packets after this point
		for mut held_packet_data in state.held_packets.drain(..) {
			if let Some(pos) = held_packet_data.windows(old_world_info_encoded.len()).position(|w| w == old_world_info_encoded) {
				let mut new_packet_data = BytesMut::from(held_packet_data);
				new_packet_data[pos..pos + old_world_info_encoded.len()].copy_from_slice(&new_world_info_encoded);
				
				held_packet_data = new_packet_data.freeze();
			}
			
			out_packets.push((held_packet_data, PacketDirection::ToClient));
		}
		
		self.phase = ServerProxyPhase::Done;
	}
}

async fn transfer_world_data(
	mut send_stream: quinn::SendStream,
	mut recv_stream: quinn::RecvStream,
	world_description: FactorioWorldDescription,
	chunks: HashMap<ChunkKey, Bytes>,
) -> anyhow::Result<()> {
	info!("Transferring world data");
	
	protocol::send_message(&mut send_stream, WorldReadyMessage {
		world: world_description,
	}).await?;
	
	let mut buffer = BytesMut::new();
	
	while let Ok(request) =
		protocol::recv_message::<RequestChunksMessage>(&mut recv_stream, &mut buffer).await
	{
		let response = SendChunksMessage {
			chunks: request.requested_chunks.iter()
				.map(|&key| chunks[&key].clone())
				.collect()
		};
		
		info!("Send chunk batch");
		
		protocol::send_message(&mut send_stream, response).await?;
	}
	
	Ok(())
}