mod proxy;
mod protocol;
mod nebula_protocol;

use crate::proxy::{client_proxy, server_proxy};
use argh::FromArgs;
use common::{quic, upnp};
use log::info;
use quinn::Endpoint;
use std::net::Ipv4Addr;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use tokio::net::{lookup_host, TcpListener};

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

#[derive(FromArgs)]
/// Run the client
#[argh(subcommand, name = "client")]
struct ClientArgs {
	#[argh(option, short = 'p', default = "60120")]
	/// port that DSP clients use to connect, defaults to 60120
	port: u16,
	
	#[argh(option, short = 'h', default = "IpAddr::V4(Ipv4Addr::UNSPECIFIED)")]
	/// host that DSP clients use to connect, defaults to 0.0.0.0
	host: IpAddr,
	
	#[argh(positional)]
	/// dsp-nebula-cacher server address in host:port form
	server_address: String,
	
	#[argh(option, short = 'c')]
	/// location of cache file, defaults to 'persistent-cache' in the CWD
	cache_path: Option<PathBuf>,
	
	#[argh(option, default = "500_000_000")]
	/// max size of the chunk cache, defaults to 500MB
	cache_limit: u64,
	
	#[argh(option, default = "60")]
	/// how often to try to save the cache in seconds, defaults to 60s
	cache_save_interval: u64,
}

#[derive(FromArgs)]
/// Run the server
#[argh(subcommand, name = "server")]
struct ServerArgs {
	#[argh(option, short = 'p', default = "60130")]
	/// port that dsp-nebula-cacher clients use to connect, defaults to 60130
	port: u16,
	
	#[argh(option, short = 'h', default = "IpAddr::V4(Ipv4Addr::UNSPECIFIED)")]
	/// host that dsp-nebula-cacher clients use to connect, defaults to 0.0.0.0
	host: IpAddr,
	
	#[argh(positional)]
	/// DSP server address in host:port form
	dsp_address: String,
	
	#[argh(switch)]
	/// enable UPNP port forwarding
	upnp: bool,
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
	let tcp_listener = TcpListener::bind(listen_address).await?;
	
	info!("Connected");
	
	let chunk_cache = common::create_chunk_cache(
		&args.cache_path,
		args.cache_limit,
		args.cache_save_interval
	).await?;
	
	info!("Listening on {}", listen_address);
	
	client_proxy::run_client_proxy(tcp_listener, quic_connection, chunk_cache).await?;
	
	Ok(())
}

async fn subcommand_server(args: ServerArgs) {
	let dsp_address = lookup_host(args.dsp_address.as_str()).await
		.expect("Error looking up host")
		.next()
		.expect("No server address found");
	
	let endpoint = quic::create_server_endpoint(SocketAddr::new(args.host, args.port));
	
	let _upnp_port_mapping = if args.upnp { Some(upnp::open_port(args.port).unwrap()) } else { None };
	
	common::cli_wrapper(&endpoint, || {
		common::run_server(&endpoint, move |conn| server_proxy::run_server_proxy(conn, dsp_address))
	}).await.unwrap();
}
