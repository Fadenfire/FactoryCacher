use crate::factorio_protocol::{FactorioPacket, FactorioPacketHeader, FactorioWorldMetadata, PacketType, ServerToClientHeartbeatPacket, TransferBlockPacket, TransferBlockRequestPacket, TRANSFER_BLOCK_SIZE};
use crate::protocol::{Datagram, WorldReadyMessage, UDP_PEER_IDLE_TIMEOUT};
use crate::proxy::{PacketDirection, UDP_QUEUE_SIZE};
use crate::factorio_world;
use anyhow::Context;
use bytes::{Bytes, BytesMut};
use common::{protocol_utils, utils};
use log::{error, info};
use memchr::memmem::Finder;
use quinn_proto::VarInt;
use std::collections::{BTreeSet, HashMap};
use std::mem;
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

				info!("New peer with id {}", peer_id);
				
                let localhost: IpAddr = if factorio_addr.is_ipv6() {
                    Ipv6Addr::LOCALHOST.into()
                } else {
                    Ipv4Addr::LOCALHOST.into()
                };

                let socket = UdpSocket::bind((localhost, 0)).await?;
				
                let (receive_queue_tx, receive_queue_rx) = mpsc::channel(UDP_QUEUE_SIZE);

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

                proxy_state.on_packet_from_server(buf.split().freeze(), &mut out_packets).await;
            }
            result = args.receive_queue_rx.recv() => {
                let Some(packet_data) = result else { return; };

                out_packets.push((packet_data, PacketDirection::ToServer));
            }
            _ = tokio::time::sleep(UDP_PEER_IDLE_TIMEOUT) => return
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
					if let Err(err) = args.socket.send_to(&packet_data, args.factorio_addr).await {
						error!("Failed to send packet to factorio server: {:?}", err);
						
						return;
					}
				}
			}
		}
	}
}

struct ServerProxyState {
	phase: ServerProxyPhase,
	packet_filter: Option<FilteringPacketsState>,
	comp_stream: Option<(quinn::SendStream, quinn::RecvStream)>,
}

enum ServerProxyPhase {
	WaitingForWorld,
	DownloadingWorld(DownloadingWorldState),
	Done,
}

struct DownloadingWorldState {
	world_info: FactorioWorldMetadata,
	new_world_info: FactorioWorldMetadata,
	world_block_count: u32,
	download_start_time: Instant,
	
	received_blocks: Vec<TransferBlockPacket>,
	block_request_queue: BTreeSet<u32>,
	inflight_block_requests: BTreeSet<u32>,
	last_block_time: Instant,
}

struct FilteringPacketsState {
	finder: Finder<'static>,
	replace_with: Bytes,
	last_replace: Instant,
}

impl ServerProxyState {
	const INFLIGHT_BLOCK_REQUEST_LIMIT: usize = 16;
	
	pub fn new(comp_stream: (quinn::SendStream, quinn::RecvStream)) -> Self {
		Self {
			phase: ServerProxyPhase::WaitingForWorld,
			packet_filter: None,
			comp_stream: Some(comp_stream),
		}
	}
	
	pub async fn on_packet_from_server(
		&mut self,
		mut in_packet_data: Bytes,
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
						let Ok(transfer_block) = TransferBlockPacket::decode(msg_data) else { return; };
						
						if state.inflight_block_requests.remove(&transfer_block.block_id) ||
							state.block_request_queue.remove(&transfer_block.block_id)
						{
							state.received_blocks.push(transfer_block);
							
							state.last_block_time = Instant::now();
						}
						
						if state.block_request_queue.is_empty() && state.inflight_block_requests.is_empty() {
							self.finalize_world();
						} else {
							Self::request_next_blocks(state, out_packets);
						}
						
						return;
					}
				}
				
				if state.last_block_time.elapsed() > Duration::from_millis(100) {
					for &block_id in &state.inflight_block_requests {
						let request = TransferBlockRequestPacket { block_id };
						out_packets.push((request.encode_full_packet(), PacketDirection::ToServer));
					}
					
					Self::request_next_blocks(state, out_packets);
					
					state.last_block_time = Instant::now();
				}
			}
			ServerProxyPhase::Done => {}
		}
		
		if let Some(filtering_state) = &mut self.packet_filter {
			in_packet_data = Self::filter_packet(filtering_state, in_packet_data);
			
			if filtering_state.last_replace.elapsed() > Duration::from_secs(30) {
				info!("Stopped filtering packets");
				
				self.packet_filter = None;
			}
		}
		
		out_packets.push((in_packet_data, PacketDirection::ToClient));
	}
	
	fn transition_to_downloading_world(
		&mut self,
		mut in_packet_data: Bytes,
		world_info: FactorioWorldMetadata,
		out_packets: &mut Vec<(Bytes, PacketDirection)>,
	) {
		info!("Got world info: {:?}", world_info);
		
		let estimated_reconstructed_world_size = world_info.world_size * 2;
		
		info!("Estimated reconstructed world size: {}", estimated_reconstructed_world_size);
		
		let new_world_info = FactorioWorldMetadata {
			world_size: estimated_reconstructed_world_size,
			..world_info
		};
		
		let mut old_world_info_encoded = Vec::new();
		let mut new_world_info_encoded = Vec::new();
		
		world_info.encode(&mut old_world_info_encoded);
		new_world_info.encode(&mut new_world_info_encoded);
		
		let mut filtering_state = FilteringPacketsState {
			finder: Finder::new(&old_world_info_encoded).into_owned(),
			replace_with: new_world_info_encoded.into(),
			last_replace: Instant::now(),
		};
		
		in_packet_data = Self::filter_packet(&mut filtering_state, in_packet_data);
		out_packets.push((in_packet_data, PacketDirection::ToClient));
		
		self.packet_filter = Some(filtering_state);
		
		let world_block_count = (world_info.world_size + TRANSFER_BLOCK_SIZE - 1) / TRANSFER_BLOCK_SIZE;
		let aux_block_count = (world_info.aux_size + TRANSFER_BLOCK_SIZE - 1) / TRANSFER_BLOCK_SIZE;
		
		let total_block_count = world_block_count + aux_block_count;
		
		let mut state = DownloadingWorldState {
			world_info,
			new_world_info,
			world_block_count,
			download_start_time: Instant::now(),
			
			received_blocks: Vec::new(),
			block_request_queue: BTreeSet::from_iter(0..total_block_count),
			inflight_block_requests: BTreeSet::new(),
			last_block_time: Instant::now(),
		};
		
		info!("Downloading world from server");
		
		Self::request_next_blocks(&mut state, out_packets);
		
		self.phase = ServerProxyPhase::DownloadingWorld(state);
	}
	
	fn request_next_blocks(state: &mut DownloadingWorldState, out_packets: &mut Vec<(Bytes, PacketDirection)>) {
		while state.inflight_block_requests.len() < Self::INFLIGHT_BLOCK_REQUEST_LIMIT {
			let Some(block_id) = state.block_request_queue.pop_first() else { return; };
			state.inflight_block_requests.insert(block_id);
			
			let request = TransferBlockRequestPacket { block_id };
			out_packets.push((request.encode_full_packet(), PacketDirection::ToServer));
		}
	}
	
	fn finalize_world(&mut self) {
		let state = match mem::replace(&mut self.phase, ServerProxyPhase::Done) {
			ServerProxyPhase::DownloadingWorld(state) => state,
			_ => unreachable!(),
		};
		
		info!("Downloading world took {}ms", state.download_start_time.elapsed().as_millis());
		
		let comp_stream = self.comp_stream.take().unwrap();
		
		tokio::spawn(async move {
			if let Err(err) = transfer_world_data(comp_stream.0, comp_stream.1, state).await {
				error!("Error trying to transfer world data: {:?}", err);
			}
		});
	}
	
	fn filter_packet(state: &mut FilteringPacketsState, packet_data: Bytes) -> Bytes {
		let mut new_packet_data = None;
		
		for pos in state.finder.find_iter(&packet_data) {
			let new_packet_data = new_packet_data
				.get_or_insert_with(|| BytesMut::from(packet_data.as_ref()));
			
			new_packet_data[pos..pos + state.finder.needle().len()].copy_from_slice(&state.replace_with);
		}
		
		if new_packet_data.is_some() {
			state.last_replace = Instant::now();
		}
		
		new_packet_data.map(BytesMut::freeze).unwrap_or(packet_data)
	}
}

async fn transfer_world_data(
	mut send_stream: quinn::SendStream,
	mut recv_stream: quinn::RecvStream,
	mut downloading_state: DownloadingWorldState,
) -> anyhow::Result<()> {
	let start_time = Instant::now();
	
	downloading_state.received_blocks.sort_by_key(|block| block.block_id);
	
	let mut received_data = BytesMut::new();
	
	for block in downloading_state.received_blocks.drain(..) {
		received_data.extend_from_slice(&block.data);
	}
	
	let received_data = received_data.freeze();
	
	let aux_data_offset = downloading_state.world_block_count * TRANSFER_BLOCK_SIZE;
	
	if received_data.len() < (aux_data_offset as usize + downloading_state.world_info.aux_size as usize) {
		return Err(anyhow::anyhow!("Received data length is smaller than expected length, received length: {}",
			received_data.len()));
	}
	
	let world_data = received_data.slice(..downloading_state.world_info.world_size as usize);
	let aux_data = received_data.slice(aux_data_offset as usize..(aux_data_offset + downloading_state.world_info.aux_size) as usize);
	
	let (world_description, chunks) =
		tokio::task::spawn_blocking(move || factorio_world::deconstruct_world(&world_data, &aux_data)).await?
			.context("Deconstruction failed")?;
	
	info!("Deconstructing world took {}ms", start_time.elapsed().as_millis());
	info!("Transferring world data");
	
	let original_world_size = downloading_state.world_info.world_size as u64;
	let mut total_transferred = 0;
	let start_time = Instant::now();
	
	let world_ready_message = protocol_utils::encode_message_async(WorldReadyMessage {
		world: world_description,
		old_info: downloading_state.world_info.clone(),
		new_info: downloading_state.new_world_info.clone(),
	}).await?;
	
	total_transferred += world_ready_message.len();
	info!("Sending world description, size: {}B", utils::abbreviate_number(world_ready_message.len() as u64));
	
	protocol_utils::write_message(&mut send_stream, world_ready_message).await?;
	
	total_transferred += protocol_utils::provide_chunks_as_requested(
		&mut send_stream,
		&mut recv_stream,
		&chunks
	).await?;
	
	let elapsed = start_time.elapsed();
	
	info!("Finished sending world in {}s, total transferred: {}B, original size: {}B, dedup ratio: {:.2}%, avg rate: {}B/s",
		elapsed.as_secs(),
		utils::abbreviate_number(total_transferred as u64),
		utils::abbreviate_number(original_world_size),
		(total_transferred as f64 / original_world_size as f64) * 100.0,
		utils::abbreviate_number((total_transferred as f64 / elapsed.as_millis() as f64 * 1000.0) as u64),
	);
	
	Ok(())
}