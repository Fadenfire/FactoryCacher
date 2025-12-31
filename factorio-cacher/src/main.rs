use crate::proxy::{client_proxy, server_proxy};
use argh::FromArgs;
use common::{cli_args, quic, upnp};
use log::info;
use quinn::Endpoint;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::net::{lookup_host, UdpSocket};

mod factorio_protocol;
mod proxy;
mod protocol;
mod zip_writer;
mod factorio_world;
mod rev_crc;
mod lan_discovery;

#[derive(FromArgs)]
/// Factorio cacher
struct Args {
	#[argh(subcommand)]
    subcommand: Subcommand,
}

#[derive(FromArgs)]
#[argh(subcommand)]
enum Subcommand {
	Client(ClientArgs),
	Server(ServerArgs),
}

cli_args::client_args! {
	#[argh(option, short = 'p', default = "60120")]
	/// port that factorio clients use to connect, defaults to 60120
	port: u16,
	
	#[argh(option, short = 'h', default = "IpAddr::V4(Ipv4Addr::UNSPECIFIED)")]
	/// host that factorio clients use to connect, defaults to 0.0.0.0
	host: IpAddr,
	
	#[argh(positional)]
	/// factorio-cacher server address in host:port form
	server_address: String,
	
	#[argh(switch)]
	/// enable UPNP port forwarding
	upnp: bool,
}

cli_args::server_args! {
	#[argh(option, short = 'p', default = "60130")]
	/// port that factorio-cacher clients use to connect, defaults to 60130
	port: u16,
	
	#[argh(option, short = 'h', default = "IpAddr::V4(Ipv4Addr::UNSPECIFIED)")]
	/// host that factorio-cacher clients use to connect, defaults to 0.0.0.0
	host: IpAddr,
	
	#[argh(positional)]
	/// factorio server address in host:port form
	factorio_address: String,
	
	#[argh(switch)]
	/// enable UPNP port forwarding
	upnp: bool,
	
	#[argh(switch)]
	/// enable auto LAN port discovery
	auto_lan: bool,
}

#[tokio::main()]
async fn main() {
	let args: Args = argh::from_env();
	
	common::setup_logging();
	
	match args.subcommand {
		Subcommand::Client(client_args) => subcommand_client(client_args).await,
		Subcommand::Server(server_args) => subcommand_server(server_args).await,
	}
}

async fn subcommand_client(args: ClientArgs) {
	let (endpoint, server_address) = quic::create_client_endpoint(&args.server_address).await;
	
	common::cli_wrapper(&endpoint, || run_client(&endpoint, server_address, &args)).await.unwrap();
}

async fn run_client(endpoint: &Endpoint, server_address: SocketAddr, args: &ClientArgs) -> anyhow::Result<()> {
	info!("Connecting...");
	
	let quic_connection = quic::client_connect(endpoint, server_address).await?;
	
	let listen_address = SocketAddr::new(args.host, args.port);
	let socket = Arc::new(UdpSocket::bind(listen_address).await?);
	
	info!("Connected");
	
	let chunk_cache = common::create_chunk_cache(args.cache_options()).await?;
	let message_transport = common::create_message_transport(args.transport_options());
	
	info!("Listening on {}", listen_address);
	
	let _upnp_port_mapping = if args.upnp { Some(upnp::open_port(args.port)?) } else { None };
	
	client_proxy::run_client_proxy(socket, quic_connection, message_transport, chunk_cache).await?;
	
	Ok(())
}

async fn subcommand_server(args: ServerArgs) {
	let factorio_address = lookup_host(args.factorio_address.as_str()).await
		.expect("Error looking up host")
		.next()
		.expect("No server address found");
	
	let endpoint = quic::create_server_endpoint(SocketAddr::new(args.host, args.port));
	let message_transport = common::create_message_transport(args.transport_options());
	
	let _upnp_port_mapping = if args.upnp { Some(upnp::open_port(args.port).unwrap()) } else { None };
	
	let factorio_address_cell = Arc::new(Mutex::new(factorio_address));
	
	if args.auto_lan {
		tokio::task::spawn(lan_discovery::lan_discovery_task(factorio_address_cell.clone()));
	}
	
	common::cli_wrapper(&endpoint, || {
		common::run_server(&endpoint, move |conn| server_proxy::run_server_proxy(
			conn,
			message_transport.clone(),
			factorio_address_cell.clone()
		))
	}).await.unwrap();
}
