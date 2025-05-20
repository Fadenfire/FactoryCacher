use crate::protocol::InitialConnectionInfo;
use crate::proxy::copy_ws_to_ws;
use bytes::Bytes;
use common::chunk_cache::ChunkCache;
use common::protocol_utils;
use fastwebsockets::upgrade::UpgradeFut;
use fastwebsockets::Role;
use http_body_util::Empty;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use log::{error, info};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::try_join;

pub async fn run_client_proxy(
	tcp_listener: TcpListener,
	connection: Arc<quinn::Connection>,
	chunk_cache: Arc<ChunkCache>,
) -> anyhow::Result<()> {
	loop {
		let (tcp_stream, addr) = tcp_listener.accept().await?;
		info!("New peer from {:?}", addr);
		
		tokio::spawn(handle_connection(tcp_stream, connection.clone(), chunk_cache.clone()));
	}
}

async fn handle_connection(tcp_stream: TcpStream, connection: Arc<quinn::Connection>, chunk_cache: Arc<ChunkCache>) {
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
	chunk_cache: Arc<ChunkCache>
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
		copy_ws_to_ws(server_ws_read, client_ws_write),
		copy_ws_to_ws(client_ws_read, server_ws_write)
	)?;
	
	Ok(())
}
