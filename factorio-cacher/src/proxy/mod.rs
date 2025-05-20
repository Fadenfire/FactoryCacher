pub mod client_proxy;
pub mod server_proxy;

pub const UDP_QUEUE_SIZE: usize = 512;

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
enum PacketDirection {
	ToClient,
	ToServer,
}
