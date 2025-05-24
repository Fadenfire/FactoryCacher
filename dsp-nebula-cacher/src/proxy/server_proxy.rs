use crate::nebula_protocol::NebulaPacketHeader;
use crate::protocol::{DedupedPacketDescription, InitialConnectionInfo, StartDedupIndicator};
use crate::proxy::{copy_ws_to_ws, read_ws_frame};
use anyhow::Context;
use bytes::{Bytes, BytesMut};
use common::{protocol_utils, utils};
use fastwebsockets::{Frame, OpCode, Payload, Role, WebSocketRead, WebSocketWrite};
use http_body_util::Empty;
use hyper::header::{CONNECTION, UPGRADE};
use hyper::{Method, Request};
use hyper_util::rt::TokioExecutor;
use log::{error, info, warn};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::time::Instant;
use tokio::try_join;

type PendingDedupStreamsMap = HashMap<u64, oneshot::Sender<(quinn::SendStream, quinn::RecvStream)>>;

pub async fn run_server_proxy(connection: Arc<quinn::Connection>, dsp_addr: SocketAddr) -> anyhow::Result<()> {
	let pending_dedup_streams: Arc<Mutex<PendingDedupStreamsMap>> = Arc::new(Mutex::new(HashMap::new()));
	
	loop {
		let (send_stream, mut recv_stream) = connection.accept_bi().await?;
		let dedup_id = recv_stream.read_u64().await?;
		
		if dedup_id == 0 {
			info!("New peer");
			
			let pending_dedup_streams = pending_dedup_streams.clone();
			
			tokio::spawn(async move {
				if let Err(err) = proxy_server(send_stream, recv_stream, dsp_addr, pending_dedup_streams).await {
					error!("Error running proxy connection: {:?}", err);
				}
			});
		} else {
			let mut pending_dedup_streams = pending_dedup_streams.lock().unwrap();
			
			let Some(tx) = pending_dedup_streams.remove(&dedup_id) else {
				return Err(anyhow::anyhow!("Client sent dedup stream with unknown id"));
			};
			
			let _ = tx.send((send_stream, recv_stream));
		}
	}
}

async fn proxy_server(
	send_stream: quinn::SendStream,
	mut recv_stream: quinn::RecvStream,
	dsp_addr: SocketAddr,
	pending_dedup_streams: Arc<Mutex<PendingDedupStreamsMap>>,
) -> anyhow::Result<()> {
	let mut buf = BytesMut::new();
	
	let message_data = protocol_utils::read_message(&mut recv_stream, &mut buf).await?;
	let initial_info: InitialConnectionInfo = protocol_utils::decode_message_async(message_data).await?;
	
	let socket = TcpStream::connect(dsp_addr).await?;
	
	let req = Request::builder()
		.method(Method::GET)
		.uri(&initial_info.client_uri)
		.header("Host", "localhost")
		.header(UPGRADE, "websocket")
		.header(CONNECTION, "upgrade")
		.header(
			"Sec-WebSocket-Key",
			fastwebsockets::handshake::generate_key(),
		)
		.header("Sec-WebSocket-Version", "13")
		.body(Empty::<Bytes>::new())
		.context("Building request")?;
	
	let (server_ws, _) = fastwebsockets::handshake::client(&TokioExecutor::new(), req, socket).await?;
	
	let (mut server_ws_read, server_ws_write) = 
		server_ws.split(tokio::io::split);
	
	server_ws_read.set_auto_close(false);
	server_ws_read.set_auto_pong(false);
	
	let (mut client_ws_read, client_ws_write) = 
		fastwebsockets::after_handshake_split(recv_stream, send_stream, Role::Server);
	
	client_ws_read.set_auto_close(false);
	client_ws_read.set_auto_pong(false);
	
	try_join!(
		copy_with_dedup(server_ws_read, client_ws_write, pending_dedup_streams),
		copy_ws_to_ws(client_ws_read, server_ws_write)
	)?;
	
	Ok(())
}

const MINIMUM_DEDUP_SIZE: usize = 128 * 1024;

async fn copy_with_dedup<R, W>(
	mut ws_read: WebSocketRead<R>,
	mut ws_write: WebSocketWrite<W>,
	pending_dedup_streams: Arc<Mutex<PendingDedupStreamsMap>>,
) -> anyhow::Result<()>
where
	R: AsyncRead + Unpin,
	W: AsyncWrite + Unpin,
{
	while let Some(frame) = read_ws_frame(&mut ws_read).await? {
		if matches!(frame.opcode, OpCode::Binary | OpCode::Text) {
			let mut frame_data = frame.payload.as_ref();
			
			match NebulaPacketHeader::decode(&mut frame_data) {
				Ok(None) => {}
				Ok(Some(packet_header)) => {
					if packet_header.approx_size() > MINIMUM_DEDUP_SIZE {
						let frame_data = frame_data.to_vec();
						let was_first_frame_final = frame.fin;
						
						dedup_packet(
							&mut ws_read,
							&mut ws_write,
							&pending_dedup_streams,
							packet_header,
							frame_data,
							was_first_frame_final
						).await.context("Deduplicating packet")?;
						
						continue;
					}
				}
				Err(err) => {
					warn!("Failed to decode Nebula packet: {:?}", err);
				}
			}
		}
		
		ws_write.write_frame(frame).await?;
	}
	
	Ok(())
}

static NEXT_DEDUP_STREAM_ID: AtomicU64 = AtomicU64::new(1);

async fn dedup_packet<R, W>(
	ws_read: &mut WebSocketRead<R>,
	ws_write: &mut WebSocketWrite<W>,
	pending_dedup_streams: &Mutex<PendingDedupStreamsMap>,
	packet_header: NebulaPacketHeader,
	initial_data: Vec<u8>,
	was_first_frame_final: bool,
) -> anyhow::Result<()>
where
	R: AsyncRead + Unpin,
	W: AsyncWrite + Unpin,
{
	info!("Deduplicating packet {:?}", &packet_header);
	
	// Receiving packet data
	
	let mut packet_data = BytesMut::from(initial_data.as_slice());
	
	if !was_first_frame_final {
		loop {
			let frame = ws_read.read_frame(&mut async |_| anyhow::Ok(())).await?;
			
			match frame.opcode {
				OpCode::Continuation => {
					packet_data.extend_from_slice(&frame.payload);
					
					if frame.fin { break; }
				}
				OpCode::Binary | OpCode::Text => {
					warn!("New WS message started before old was finished");
					
					ws_write.write_frame(frame).await?;
					
					return Ok(());
				}
				_ => { // Control frame
					ws_write.write_frame(frame).await?;
				}
			}
		}
	}
	
	let packet_data = packet_data.freeze();
	let original_packet_size = packet_data.len() as u64;
	
	// Deconstructing packet
	
	let (deconstructed_packet, all_chunks) = tokio::task::spawn_blocking(move || {
		let mut all_chunks = HashMap::new();
		let packet = packet_header.deconstruct(packet_data, &mut all_chunks)?;
		
		anyhow::Ok((packet, all_chunks))
	}).await?.context("Deconstructing packet")?;
	
	// Establishing channel with client
	
	let (tx, rx) = oneshot::channel();
	let stream_id = NEXT_DEDUP_STREAM_ID.fetch_add(1, Ordering::SeqCst);
	
	pending_dedup_streams.lock().unwrap().insert(stream_id, tx);
	
	let mut buf = BytesMut::new();
	StartDedupIndicator { dedup_id: stream_id }.encode(&mut buf);
	
	ws_write.write_frame(Frame::binary(Payload::Bytes(buf))).await?;
	
	let (mut send_stream, mut recv_stream) = rx.await?;
	
	// Sending packet description
	
	let mut total_transferred = 0;
	let start_time = Instant::now();
	
	let message_data = protocol_utils::encode_message_async(DedupedPacketDescription {
		packet: deconstructed_packet,
		original_packet_size,
	}).await?;
	
	total_transferred += message_data.len();
	info!("Sending packet description, size: {}B", utils::abbreviate_number(message_data.len() as u64));
	
	protocol_utils::write_message(&mut send_stream, message_data).await?;
	
	// Sending chunks
	
	total_transferred += protocol_utils::provide_chunks_as_requested(
		&mut send_stream,
		&mut recv_stream,
		&all_chunks
	).await?;
	
	let elapsed = start_time.elapsed();
	
	info!("Finished sending packet in {}s, total transferred: {}B, original size: {}B, dedup ratio: {:.2}%, avg rate: {}B/s",
		elapsed.as_secs(),
		utils::abbreviate_number(total_transferred as u64),
		utils::abbreviate_number(original_packet_size),
		(total_transferred as f64 / original_packet_size as f64) * 100.0,
		utils::abbreviate_number((total_transferred as f64 / elapsed.as_millis() as f64 * 1000.0) as u64),
	);
	
	Ok(())
}