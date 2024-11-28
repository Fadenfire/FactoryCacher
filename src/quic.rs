use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use std::sync::Arc;
use std::time::Duration;
use rustls::pki_types::pem::PemObject;

pub const QUIC_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
pub const QUIC_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);

const CERT_DATA: &[u8] = include_bytes!("../certs/cert.pem");
const PRIV_KEY_DATA: &[u8] = include_bytes!("../certs/key.pem");

pub fn make_client_config() -> quinn::ClientConfig {
	let mut certs = rustls::RootCertStore::empty();
	certs.add(CertificateDer::from_pem_slice(CERT_DATA).unwrap()).unwrap();
	
	let mut client_config = quinn::ClientConfig::with_root_certificates(Arc::new(certs)).unwrap();
	
	let mut transport_config = quinn::TransportConfig::default();
	transport_config.max_idle_timeout(Some(QUIC_IDLE_TIMEOUT.try_into().unwrap()));
	transport_config.keep_alive_interval(Some(QUIC_KEEPALIVE_INTERVAL));
	
	client_config.transport_config(Arc::new(transport_config));
	
	client_config
}

pub fn make_server_config() -> quinn::ServerConfig {
	let cert = CertificateDer::from_pem_slice(CERT_DATA).unwrap();
	let priv_key = PrivatePkcs8KeyDer::from_pem_slice(PRIV_KEY_DATA).unwrap();
	
	let mut server_config = quinn::ServerConfig::with_single_cert(vec![cert], priv_key.into()).unwrap();
	
	let mut transport_config = quinn::TransportConfig::default();
	transport_config.max_idle_timeout(Some(QUIC_IDLE_TIMEOUT.try_into().unwrap()));
	
	server_config.transport_config(Arc::new(transport_config));
	
	server_config
}
