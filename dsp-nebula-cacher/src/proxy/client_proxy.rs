use crate::protocol::{DedupedPacketDescription, InitialConnectionInfo, StartDedupIndicator};
use crate::proxy::{copy_ws_to_ws, read_ws_frame};
use bytes::{Bytes, BytesMut};
use common::chunk_cache::ChunkCache;
use common::protocol_utils::{ChunkBatchFetcher, ChunkFetcherProvider};
use common::{protocol_utils, utils};
use fastwebsockets::upgrade::UpgradeFut;
use fastwebsockets::{Frame, OpCode, Payload, Role, WebSocketRead, WebSocketWrite};
use http_body_util::Empty;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use log::{error, info, };
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio::{select, try_join};

pub async fn run_client_proxy(
	tcp_listener: TcpListener,
	connection: Arc<quinn::Connection>,
	chunk_cache: Arc<ChunkCache>,
) -> anyhow::Result<()> {
	loop {
		select! {
			result = tcp_listener.accept() => {
				let (tcp_stream, addr) = result?;
				info!("New peer from {:?}", addr);
				
				tokio::spawn(handle_connection(tcp_stream, connection.clone(), chunk_cache.clone()));
			}
			_ = connection.closed() => {
				info!("Connection closed");
				
				return Ok(());
			}
		}
	}
}

async fn handle_connection(tcp_stream: TcpStream, connection: Arc<quinn::Connection>, chunk_cache: Arc<ChunkCache>) {
	tcp_stream.set_nodelay(true).expect("Error disabling Nagle algorithm");
	
	let service = service_fn(|req| {
		handle_request(req, connection.clone(), chunk_cache.clone())
	});
	
	let result = hyper::server::conn::http1::Builder::new()
		.serve_connection(TokioIo::new(tcp_stream), service)
		.with_upgrades()
		.await;
	
	if let Err(err) = result {
		error!("Error handling client connection: {:?}", err);
	}
}

async fn handle_request(
	mut request: Request<Incoming>,
	connection: Arc<quinn::Connection>,
	chunk_cache: Arc<ChunkCache>,
) -> anyhow::Result<Response<Empty<Bytes>>> {
	if !fastwebsockets::upgrade::is_upgrade_request(&request) {
		return Ok(Response::builder()
			.status(StatusCode::BAD_REQUEST)
			.body(Empty::new())?);
	}
	
	let (response, ws_future) = fastwebsockets::upgrade::upgrade(&mut request)?;
	
	let (mut send_stream, recv_stream) = connection.open_bi().await?;
	
	let message_data = protocol_utils::encode_message_async(InitialConnectionInfo {
		client_uri: request.uri().to_string(),
	}).await?;
	
	send_stream.write_u64(0).await?; // Indicate this isn't a dedup stream
	protocol_utils::write_message(&mut send_stream, message_data).await?;
	
	tokio::spawn(async move {
		if let Err(err) = handle_ws_connection(ws_future, connection, send_stream, recv_stream, chunk_cache).await {
			error!("Error handling websocket connection: {:?}", err);
		}
	});
	
	Ok(response)
}

async fn handle_ws_connection(
	ws_future: UpgradeFut,
	connection: Arc<quinn::Connection>,
	send_stream: quinn::SendStream,
	recv_stream: quinn::RecvStream,
	chunk_cache: Arc<ChunkCache>,
) -> anyhow::Result<()> {
	let client_ws = ws_future.await?;
	
	let (mut client_ws_read, client_ws_write) =
		client_ws.split(tokio::io::split);
	
	client_ws_read.set_auto_close(false);
	client_ws_read.set_auto_pong(false);
	
	let (mut server_ws_read, server_ws_write) =
		fastwebsockets::after_handshake_split(recv_stream, send_stream, Role::Client);
	
	server_ws_read.set_auto_close(false);
	server_ws_read.set_auto_pong(false);
	
	try_join!(
		copy_with_dedup(server_ws_read, client_ws_write, connection, chunk_cache),
		copy_ws_to_ws(client_ws_read, server_ws_write)
	)?;
	
	info!("Peer disconnected");
	
	Ok(())
}

async fn copy_with_dedup<R, W>(
	mut ws_read: WebSocketRead<R>,
	mut ws_write: WebSocketWrite<W>,
	connection: Arc<quinn::Connection>,
	chunk_cache: Arc<ChunkCache>,
) -> anyhow::Result<()>
where
	R: AsyncRead + Unpin,
	W: AsyncWrite + Unpin,
{
	while let Some(frame) = read_ws_frame(&mut ws_read).await? {
		if matches!(frame.opcode, OpCode::Binary | OpCode::Text) && frame.fin {
			if let Some(indicator) = StartDedupIndicator::decode(frame.payload.as_ref()) {
				let (tx, mut rx) = mpsc::channel(16);
				
				let connection = connection.clone();
				let chunk_cache = chunk_cache.clone();
				
				tokio::spawn(async move {
					if let Err(err) = reconstruct_packet(indicator.dedup_id, tx, connection, chunk_cache).await {
						error!("Error reconstructing packet: {:?}", err);
					}
				});
				
				let mut first_frame = true;
				let mut last_bytes: Option<Bytes> = None;
				
				while let Some(data) = rx.recv().await {
					if let Some(last_bytes) = last_bytes.take() {
						let op_code = if first_frame { OpCode::Binary } else { OpCode::Continuation };
						
						ws_write.write_frame(Frame::new(false, op_code, None, Payload::Borrowed(&last_bytes))).await?;
						
						first_frame = false;
					}
					
					last_bytes = Some(data);
				}
				
				if let Some(last_bytes) = last_bytes.take() {
					let op_code = if first_frame { OpCode::Binary } else { OpCode::Continuation };
					
					ws_write.write_frame(Frame::new(true, op_code, None, Payload::Borrowed(&last_bytes))).await?;
				}
				
				continue;
			}
		}
		
		ws_write.write_frame(frame).await?;
	}
	
	Ok(())
}

async fn reconstruct_packet(
	dedup_stream_id: u64,
	outgoing_packets: mpsc::Sender<Bytes>,
	connection: Arc<quinn::Connection>,
	chunk_cache: Arc<ChunkCache>,
) -> anyhow::Result<()> {
	let mut buf = BytesMut::new();
	
	info!("Receiving deduplicated packet");
	
	// Open stream
	
	let (mut send_stream, mut recv_stream) = connection.open_bi().await?;
	send_stream.write_u64(dedup_stream_id).await?;
	
	// Read desc
	
	let mut total_transferred = 0;
	let start_time = Instant::now();
	
	let packet_desc_message_data = protocol_utils::read_message(&mut recv_stream, &mut buf).await?;
	
	total_transferred += packet_desc_message_data.len();
	info!("Received packet description, size: {}B", utils::abbreviate_number(packet_desc_message_data.len() as u64));
	
	let packet_desc: DedupedPacketDescription = protocol_utils::decode_message_async(packet_desc_message_data).await?;
	
	// Fetch chunks
	
	let mut chunk_fetcher = ChunkFetcherProvider::new(&chunk_cache, &mut send_stream, &mut recv_stream);
	chunk_fetcher.set_chunks_remaining(packet_desc.packet.required_chunks());
	
	
	
	let elapsed = start_time.elapsed();
	
	info!("Finished receiving packet in {}s, total transferred: {}B, original size: {}B, dedup ratio: {:.2}%",
		elapsed.as_secs(),
		utils::abbreviate_number(total_transferred as u64),
		utils::abbreviate_number(packet_desc.original_packet_size),
		(total_transferred as f64 / packet_desc.original_packet_size as f64) * 100.0,
	);
	
	chunk_cache.mark_dirty();
	
	// Reconstruct
	
	let packet = packet_desc.packet;
	let reconstructed_data = tokio::task::spawn(packet.reconstruct(&mut chunk_fetcher)).await??;
	
	for fragment in reconstructed_data.chunks(2048) {
		if outgoing_packets.send(fragment.to_vec().into()).await.is_err() {
			info!("Peer disconnected while sending reconstructed packet");
			
			break;
		}
	}
	
	Ok(())
}