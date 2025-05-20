use crate::protocol::InitialConnectionInfo;
use crate::proxy::copy_ws_to_ws;
use anyhow::Context;
use bytes::{Bytes, BytesMut};
use common::protocol_utils;
use fastwebsockets::Role;
use http_body_util::Empty;
use hyper::header::{CONNECTION, UPGRADE};
use hyper::{Method, Request};
use hyper_util::rt::TokioExecutor;
use log::{error, info};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::try_join;

type PendingDedupStreamsMap = HashMap<u64, oneshot::Sender<(quinn::SendStream, quinn::RecvStream)>>;

pub async fn run_server_proxy(connection: Arc<quinn::Connection>, dsp_addr: SocketAddr) -> anyhow::Result<()> {
	let mut pending_dedup_streams: Arc<Mutex<PendingDedupStreamsMap>> =
		Arc::new(Mutex::new(HashMap::new()));
	
	loop {
		let (send_stream, recv_stream) = connection.accept_bi().await?;
		
		{
			let mut pending_dedup_streams = pending_dedup_streams.lock().unwrap();
			
			if let Some(tx) = pending_dedup_streams.remove(&recv_stream.id().0) {
				let _ = tx.send((send_stream, recv_stream));
				
				continue;
			}
		}
		
		info!("New peer");
		
		let pending_dedup_streams = pending_dedup_streams.clone();
		
		tokio::spawn(async move {
			if let Err(err) = proxy_server(send_stream, recv_stream, dsp_addr, pending_dedup_streams).await {
				error!("Error running proxy connection: {:?}", err);
			}
		});
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
		copy_ws_to_ws(server_ws_read, client_ws_write),
		copy_ws_to_ws(client_ws_read, server_ws_write)
	)?;
	
	Ok(())
}
