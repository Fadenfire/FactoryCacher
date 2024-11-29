pub mod client_proxy;
pub mod server_proxy;

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
enum PacketDirection {
	ToClient,
	ToServer,
}
