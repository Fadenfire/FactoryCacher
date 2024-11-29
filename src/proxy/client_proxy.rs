use crate::chunk_cache::ChunkCache;
use crate::dedup::WorldReconstructor;
use crate::factorio_protocol::{FactorioPacket, FactorioPacketHeader, PacketType, TransferBlockPacket, TransferBlockRequestPacket, TRANSFER_BLOCK_SIZE};
use crate::protocol;
use crate::protocol::{Datagram, RequestChunksMessage, SendChunksMessage, WorldReadyMessage};
use crate::proxy::PacketDirection;
use anyhow::anyhow;
use bytes::{Bytes, BytesMut};
use log::{error, info};
use quinn_proto::VarInt;
use std::collections::HashMap;
use std::mem;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::select;
use tokio::sync::mpsc;

pub async fn run_client_proxy(
	socket: Arc<UdpSocket>,
	connection: Arc<quinn::Connection>,
	chunk_cache: Arc<ChunkCache>,
) -> anyhow::Result<()> {
	let mut addr_to_queue: HashMap<SocketAddr, mpsc::Sender<Bytes>> = HashMap::new();
	let mut id_to_queue: HashMap<VarInt, mpsc::Sender<Bytes>> = HashMap::new();
	
	let mut buffer = BytesMut::new();
	let mut next_peer_id: u32 = 0;
	
	loop {
		buffer.clear();
		buffer.reserve(8192);
		
		select! {
			result = socket.recv_buf_from(&mut buffer) => {
				let peer_addr = result?.1;
				
				let outgoing_queue = match addr_to_queue.get(&peer_addr).filter(|s| !s.is_closed()) {
					Some(sender) => sender,
					None => {
						let peer_id: VarInt = next_peer_id.into();
						next_peer_id = next_peer_id.checked_add(1).ok_or_else(|| anyhow!("Ran out of peer ids"))?;
						
						let (server_receive_queue_tx, server_receive_queue_rx) = mpsc::channel(8192);
						let (client_receive_queue_tx, client_receive_queue_rx) = mpsc::channel(8192);
						
						tokio::spawn(proxy_client(ProxyClientArgs {
							connection: connection.clone(),
							peer_id,
							
							socket: socket.clone(),
							peer_addr,
							
							server_receive_queue: server_receive_queue_rx,
							client_receive_queue: client_receive_queue_rx,
							chunk_cache: chunk_cache.clone(),
						}));
						
						addr_to_queue.insert(peer_addr, client_receive_queue_tx);
						id_to_queue.insert(peer_id, server_receive_queue_tx);
						
						addr_to_queue.get(&peer_addr).unwrap()
					}
				};
				
				let _ = outgoing_queue.try_send(buffer.split().freeze());
			},
			result = connection.read_datagram() => {
				let datagram = Datagram::decode(result?)?;
				
				if let Some(outgoing_queue) = id_to_queue.get(&datagram.peer_id) {
					let _ = outgoing_queue.try_send(datagram.data);
				}
			}
		}
	}
}

struct ProxyClientArgs {
	connection: Arc<quinn::Connection>,
	peer_id: VarInt,
	
	socket: Arc<UdpSocket>,
	peer_addr: SocketAddr,
	
	server_receive_queue: mpsc::Receiver<Bytes>,
	client_receive_queue: mpsc::Receiver<Bytes>,
	chunk_cache: Arc<ChunkCache>,
}

async fn proxy_client(mut args: ProxyClientArgs) {
	let (mut comp_send, comp_recv) = args.connection.open_bi().await.unwrap();
	
	comp_send.write_all(&(args.peer_id.into_inner() as u32).to_le_bytes()).await.unwrap();
	
	let (world_data_sender, mut world_data_receiver) = mpsc::channel(32);
	
	tokio::spawn(async {
		if let Err(err) = transfer_world_data(comp_send, comp_recv, world_data_sender, args.chunk_cache).await {
			error!("Error trying to transfer world data: {:?}", err);
		}
	});
	
	let mut buf = BytesMut::new();
	let mut out_packets = Vec::new();
	
	let mut proxy_state = ClientProxyState::new();
	let mut world_data_done = false;
	
	loop {
		select! {
			result = args.client_receive_queue.recv() => {
				let Some(packet_data) = result else { return; };
				
				proxy_state.on_packet_from_client(packet_data, &mut out_packets);
				// out_packets.push((packet_data, PacketDirection::ToServer));
			}
			result = args.server_receive_queue.recv() => {
				let Some(packet_data) = result else { return; };
				
				out_packets.push((packet_data, PacketDirection::ToClient));
			}
			result = world_data_receiver.recv(), if !world_data_done => {
				let Some(new_data) = result else {
					world_data_done = true;
					continue;
				};
				
				proxy_state.on_new_world_data(new_data, &mut out_packets);
			}
			// _ = tokio::time::sleep(UDP_PEER_IDLE_TIMEOUT) => return
		}
		
		for (packet_data, dir) in out_packets.drain(..) {
			match dir {
				PacketDirection::ToClient => {
					if args.socket.send_to(&packet_data, args.peer_addr).await.is_err() {
						return;
					}
				}
				PacketDirection::ToServer => {
					Datagram::new(args.peer_id, packet_data).encode(&mut buf);
					
					if args.connection.send_datagram(buf.split().freeze()).is_err() {
						return;
					}
				}
			}
		}
	}
}

struct ClientProxyState {
	world_data: Vec<u8>,
	pending_requests: Vec<TransferBlockRequestPacket>,
	pending_requests_swap: Vec<TransferBlockRequestPacket>,
}

impl ClientProxyState {
	pub fn new() -> Self {
		Self {
			world_data: Vec::new(),
			pending_requests: Vec::new(),
			pending_requests_swap: Vec::new(),
		}
	}
	
	pub fn on_packet_from_client(&mut self, packet_data: Bytes, out_packets: &mut Vec<(Bytes, PacketDirection)>) {
		if let Ok((header, msg_data)) = FactorioPacketHeader::decode(packet_data.clone()) {
			if header.packet_type == PacketType::TransferBlockRequest {
				let Ok(request) = TransferBlockRequestPacket::decode(msg_data)
					else { return; };
				
				if let Some(response) = self.try_fulfill_block_request(&request) {
					out_packets.push((response.encode_full_packet(), PacketDirection::ToClient));
				} else {
					self.pending_requests.push(request);
				}
				
				return;
			}
		}
		
		out_packets.push((packet_data, PacketDirection::ToServer));
	}
	
	pub fn on_new_world_data(&mut self, new_data: Bytes, out_packets: &mut Vec<(Bytes, PacketDirection)>) {
		self.world_data.extend_from_slice(&new_data);
		
		while let Some(request) = self.pending_requests.pop() {
			if let Some(response) = self.try_fulfill_block_request(&request) {
				out_packets.push((response.encode_full_packet(), PacketDirection::ToClient));
			} else {
				self.pending_requests_swap.push(request);
			}
		}
		
		mem::swap(&mut self.pending_requests, &mut self.pending_requests_swap);
	}
	
	fn try_fulfill_block_request(&self, request: &TransferBlockRequestPacket) -> Option<TransferBlockPacket> {
		let offset = request.block_id as usize * TRANSFER_BLOCK_SIZE as usize;
		
		if offset + TRANSFER_BLOCK_SIZE as usize <= self.world_data.len() {
			Some(TransferBlockPacket {
				block_id: request.block_id,
				data: self.world_data[offset..offset + TRANSFER_BLOCK_SIZE as usize].to_vec().into(),
			})
		} else {
			None
		}
	}
}

async fn transfer_world_data(
	mut send_stream: quinn::SendStream,
	mut recv_stream: quinn::RecvStream,
	world_data_sender: mpsc::Sender<Bytes>,
	chunk_cache: Arc<ChunkCache>,
) -> anyhow::Result<()> {
	let mut buf = BytesMut::new();
	
	let world_ready: WorldReadyMessage = protocol::recv_message(&mut recv_stream, &mut buf).await?;
	let world_desc = world_ready.world;
	
	info!("Got world description: size: {}, crc: {}, file count: {}", world_desc.reconstructed_size, world_desc.reconstructed_crc, world_desc.files.len());
	
	let mut all_chunks = world_desc.files.iter()
		.flat_map(|file| file.content_chunks.iter())
		.copied()
		.collect::<Vec<_>>();
	
	let mut local_cache = HashMap::new();
	let mut world_reconstructor = WorldReconstructor::new();
	
	for file_desc in &world_desc.files {
		info!("Reconstructing file {}", &file_desc.file_name);
		
		loop {
			match world_reconstructor.reconstruct_world_file(file_desc, &mut local_cache, &mut buf) {
				Ok(data_blocks) => {
					world_data_sender.send(data_blocks.0).await?;
					world_data_sender.send(data_blocks.1).await?;
					
					break;
				}
				Err(_) => {
					if all_chunks.is_empty() {
						panic!("Emptied chunk list but reconstructor wants more data");
					}
					
					if let Some(batch) =
						chunk_cache.get_chunks_batched(&mut all_chunks, &mut local_cache, 512).await
					{
						protocol::send_message(&mut send_stream, RequestChunksMessage {
							requested_chunks: batch.batch_keys().to_vec(),
						}).await?;
						
						let response: SendChunksMessage = protocol::recv_message(&mut recv_stream, &mut buf).await?;
						
						for (&key, chunk) in batch.batch_keys().iter().zip(response.chunks.iter()) {
							local_cache.insert(key, chunk.clone());
						}
						
						batch.fulfill(&response.chunks);
					}
				}
			}
		}
	}
	
	info!("Sending final data");
	
	let last_data = world_reconstructor.finalize_world_file(&world_desc)?;
	world_data_sender.send(last_data).await?;
	
	Ok(())
}