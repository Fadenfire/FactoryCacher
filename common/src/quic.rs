use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use std::sync::Arc;
use std::time::Duration;
use quinn::Endpoint;
use rustls::pki_types::pem::PemObject;
use tokio::net::lookup_host;

pub const QUIC_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
pub const QUIC_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);

const ROOT_CERT_DATA: &[u8] = include_bytes!("../../certs/root-ca.pem");

const END_CERT_DATA: &[u8] = include_bytes!("../../certs/cert.pem");
const END_PRIVATE_KEY_DATA: &[u8] = include_bytes!("../../certs/cert.key.pem");

pub fn make_client_config() -> quinn::ClientConfig {
	let mut certs = rustls::RootCertStore::empty();
	certs.add(CertificateDer::from_pem_slice(ROOT_CERT_DATA).unwrap()).unwrap();
	
	let mut client_config = quinn::ClientConfig::with_root_certificates(Arc::new(certs)).unwrap();
	
	let mut transport_config = quinn::TransportConfig::default();
	transport_config.max_idle_timeout(Some(QUIC_IDLE_TIMEOUT.try_into().unwrap()));
	transport_config.keep_alive_interval(Some(QUIC_KEEPALIVE_INTERVAL));
	
	client_config.transport_config(Arc::new(transport_config));
	
	client_config
}

pub fn make_server_config() -> quinn::ServerConfig {
	let cert = CertificateDer::from_pem_slice(END_CERT_DATA).unwrap();
	let private_key = PrivatePkcs8KeyDer::from_pem_slice(END_PRIVATE_KEY_DATA).unwrap();
	
	let mut server_config = quinn::ServerConfig::with_single_cert(vec![cert], private_key.into()).unwrap();
	
	let mut transport_config = quinn::TransportConfig::default();
	transport_config.max_idle_timeout(Some(QUIC_IDLE_TIMEOUT.try_into().unwrap()));
	
	server_config.transport_config(Arc::new(transport_config));
	
	server_config
}

pub async fn create_client_endpoint(server_address: &str) -> (Endpoint, SocketAddr) {
	let server_address = lookup_host(server_address).await
		.expect("Error looking up host")
		.next()
		.expect("No server address found");
	
	let local_address = SocketAddr::new(if server_address.is_ipv6() {
		Ipv6Addr::UNSPECIFIED.into()
	} else {
		Ipv4Addr::UNSPECIFIED.into()
	}, 0);
	
	let mut endpoint = Endpoint::client(local_address).unwrap();
	endpoint.set_default_client_config(make_client_config());
	
	(endpoint, server_address)
}
