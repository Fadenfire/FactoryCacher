use crate::factorio_protocol::{FactorioPacket, FactorioPacketHeader, PacketType, TransferBlockPacket, TransferBlockRequestPacket, TRANSFER_BLOCK_SIZE};
use crate::factorio_world::WorldReconstructor;
use crate::protocol::{Datagram, WorldReadyMessage, UDP_PEER_IDLE_TIMEOUT};
use crate::proxy::{PacketDirection, UDP_QUEUE_SIZE};
use anyhow::anyhow;
use bytes::{Bytes, BytesMut};
use common::chunk_cache::ChunkCache;
use common::protocol_utils::ChunkFetcherProvider;
use common::{dedup, protocol_utils, utils};
use log::{debug, error, info};
use quinn_proto::VarInt;
use std::collections::{BTreeSet, HashMap};
use std::io::ErrorKind;
use std::mem;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::UdpSocket;
use tokio::select;
use tokio::sync::mpsc;
use tokio::time::Instant;

const WORLD_DATA_TIMEOUT: Duration = Duration::from_secs(60);

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
						
						info!("New peer from {} with id {}", peer_addr, peer_id);
						
						let (server_receive_queue_tx, server_receive_queue_rx) = mpsc::channel(UDP_QUEUE_SIZE);
						let (client_receive_queue_tx, client_receive_queue_rx) = mpsc::channel(UDP_QUEUE_SIZE);
						
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
	let result: anyhow::Result<_> = async {
		let (mut comp_send, comp_recv) = args.connection.open_bi().await?;
		comp_send.write_u32_le(args.peer_id.into_inner() as u32).await?;
		
		let (world_data_sender, world_data_receiver) = mpsc::channel(32);
		
		tokio::spawn(async {
			if let Err(err) = transfer_world_data(comp_send, comp_recv, world_data_sender, args.chunk_cache).await {
				error!("Error trying to transfer world data: {:?}", err);
			}
		});
		
		Ok(world_data_receiver)
	}.await;
	
	let mut world_data_receiver = match result {
		Ok(r) => r,
		Err(err) => {
			error!("Error initializing stream: {:?}", err);
			return;
		}
	};
	
	let mut buf = BytesMut::new();
	let mut out_packets = Vec::new();
	
	let mut proxy_state = ClientProxyState::new();
	let mut world_data_done = false;
	
	loop {
		select! {
			result = args.client_receive_queue.recv() => {
				let Some(packet_data) = result else { return; };
				
				proxy_state.on_packet_from_client(packet_data, &mut out_packets);
			}
			result = args.server_receive_queue.recv() => {
				let Some(packet_data) = result else { return; };
				
				out_packets.push((packet_data, PacketDirection::ToClient));
			}
			result = world_data_receiver.recv(), if !world_data_done => {
				if result.is_none() {
					world_data_done = true;
				}
				
				proxy_state.on_new_world_data(result, &mut out_packets);
			}
			_ = tokio::time::sleep(UDP_PEER_IDLE_TIMEOUT) => return
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
	last_block_request: Instant,
	pending_requests: BTreeSet<u32>,
	pending_requests_swap: BTreeSet<u32>,
	world_data_done: bool,
}

impl ClientProxyState {
	pub fn new() -> Self {
		Self {
			world_data: Vec::new(),
			last_block_request: Instant::now(),
			pending_requests: BTreeSet::new(),
			pending_requests_swap: BTreeSet::new(),
			world_data_done: false,
		}
	}
	
	pub fn on_packet_from_client(&mut self, packet_data: Bytes, out_packets: &mut Vec<(Bytes, PacketDirection)>) {
		if let Ok((header, msg_data)) = FactorioPacketHeader::decode(packet_data.clone()) {
			if header.packet_type == PacketType::TransferBlockRequest {
				if let Ok(request) = TransferBlockRequestPacket::decode(msg_data) {
					if let Some(response) = self.try_fulfill_block_request(request.block_id) {
						out_packets.push((response.encode_full_packet(), PacketDirection::ToClient));
					} else {
						self.pending_requests.insert(request.block_id);
					}
					
					self.last_block_request = Instant::now();
					return;
				}
			}
		}
		
		if !self.world_data.is_empty() && self.world_data_done && self.last_block_request.elapsed() > WORLD_DATA_TIMEOUT {
			info!("Cleaning up local copy of world data");
			
			self.world_data = Vec::new();
		}
		
		out_packets.push((packet_data, PacketDirection::ToServer));
	}
	
	pub fn on_new_world_data(&mut self, new_data: Option<Bytes>, out_packets: &mut Vec<(Bytes, PacketDirection)>) {
		let Some(new_data) = new_data else {
			self.world_data_done = true;
			self.last_block_request = Instant::now();
			
			return;
		};
		
		self.world_data.extend_from_slice(&new_data);
		
		for &requested_block_id in &self.pending_requests {
			if let Some(response) = self.try_fulfill_block_request(requested_block_id) {
				out_packets.push((response.encode_full_packet(), PacketDirection::ToClient));
			} else {
				self.pending_requests_swap.insert(requested_block_id);
			}
		}
		
		self.pending_requests.clear();
		
		self.last_block_request = Instant::now();
		mem::swap(&mut self.pending_requests, &mut self.pending_requests_swap);
	}
	
	fn try_fulfill_block_request(&self, requested_block_id: u32) -> Option<TransferBlockPacket> {
		let offset = requested_block_id as usize * TRANSFER_BLOCK_SIZE as usize;
		
		if offset + TRANSFER_BLOCK_SIZE as usize <= self.world_data.len() {
			Some(TransferBlockPacket {
				block_id: requested_block_id,
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
	let start_time = Instant::now();
	let mut total_transferred = 0;
	
	let mut buf = BytesMut::new();
	
	// Receive world description
	
	let world_ready_message_data = match protocol_utils::read_message(&mut recv_stream, &mut buf).await {
		Ok(msg_data) => msg_data,
		Err(err) if err.downcast_ref::<std::io::Error>().is_some_and(|err| err.kind() == ErrorKind::UnexpectedEof) => {
			info!("Peer shutdown without ever sending world data");
			
			return Ok(());
		}
		Err(err) => return Err(err.into()),
	};
	
	total_transferred += world_ready_message_data.len();
	
	info!("Received world description, size: {}B", utils::abbreviate_number(world_ready_message_data.len() as u64));
	
	let world_ready: WorldReadyMessage = protocol_utils::decode_message_async(world_ready_message_data).await?;
	let world_desc = world_ready.world;
	
	info!("World description: size: {}, crc: {}, file count: {}, total chunks: {}",
		world_ready.new_info.world_size, world_ready.new_info.world_crc, world_desc.files.len(), world_desc.total_chunks);
	
	// Reconstruct world, fetching world data chunks along the way
	
	let mut chunk_fetcher = ChunkFetcherProvider::new(&chunk_cache, &mut send_stream, &mut recv_stream);
	
	dedup::prefetch_chunks(
		world_desc.files.iter()
			.map(|file| file.chunk_list.clone())
			.collect(),
		&mut chunk_fetcher,
	).await?;
	
	let mut world_reconstructor = WorldReconstructor::new();
	
	for file_desc in &world_desc.files {
		debug!("Reconstructing file {}", &file_desc.file_name);
		
		let data_blocks = world_reconstructor.reconstruct_world_file(
			file_desc,
			&mut chunk_fetcher
		).await?;
		
		for data in data_blocks {
			world_data_sender.send(data).await?;
		}
	}
	
	total_transferred += chunk_fetcher.total_transferred();
	
	let elapsed = start_time.elapsed();
	
	info!("Finished receiving world in {}s, total transferred: {}B, original size: {}B, dedup ratio: {:.2}%, avg rate: {}B/s",
		elapsed.as_secs(),
		utils::abbreviate_number(total_transferred as u64),
		utils::abbreviate_number(world_ready.old_info.world_size as u64),
		(total_transferred as f64 / world_ready.old_info.world_size as f64) * 100.0,
		utils::abbreviate_number((total_transferred as f64 / elapsed.as_millis() as f64 * 1000.0) as u64),
	);
	
	chunk_cache.mark_dirty();
	
	info!("Reconstructing final data");
	
	let last_data = world_reconstructor.finalize_world(
		&world_desc,
		world_ready.new_info.world_size as usize,
		world_ready.new_info.world_crc
	)?;
	
	world_data_sender.send(last_data).await?;
	
	Ok(())
}