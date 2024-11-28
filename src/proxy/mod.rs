use bytes::Bytes;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

pub mod client_proxy;
pub mod server_proxy;

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
enum PacketDirection {
	ToClient,
	ToServer,
}

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
enum PacketAction {
	Pass,
	Custom,
}

trait PacketSender {
	async fn send_packet(&self, packet_data: Bytes) -> anyhow::Result<()>;
}

pub struct UdpSocketSender {
	socket: Arc<UdpSocket>,
	dest_addr: SocketAddr,
}

impl UdpSocketSender {
	pub fn new(socket: Arc<UdpSocket>, dest_addr: SocketAddr) -> Self {
		Self { socket, dest_addr }
	}
}

impl PacketSender for UdpSocketSender {
	async fn send_packet(&self, packet_data: Bytes) -> anyhow::Result<()> {
		self.socket.send_to(&packet_data, self.dest_addr).await?;
		Ok(())
	}
}

pub struct QueueSender {
	queue: mpsc::Sender<Bytes>,
}

impl QueueSender {
	pub fn new(queue: mpsc::Sender<Bytes>) -> Self {
		Self { queue }
	}
}

impl PacketSender for QueueSender {
	async fn send_packet(&self, packet_data: Bytes) -> anyhow::Result<()> {
		match self.queue.try_send(packet_data) {
			Ok(_) => {},
			Err(TrySendError::Full(_)) => {}, // Drop packet if the queue is full
			err @ Err(TrySendError::Closed(_)) => err?,
		}
		
		Ok(())
	}
}