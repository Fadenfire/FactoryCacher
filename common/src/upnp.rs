use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use anyhow::Context;
use igd_next::{PortMappingProtocol, SearchOptions};
use log::{error, info};
use crate::utils;

pub const LEASE_DURATION: u32 = 24 * 60 * 60;

pub struct UpnpPortMapping {
	gateway: igd_next::Gateway,
	port: u16,
}

pub fn open_port(port: u16) -> anyhow::Result<UpnpPortMapping> {
	let gateway = igd_next::search_gateway(SearchOptions::default())
		.context("Finding gateway")?;
	
	let local_ip = utils::get_local_ip().context("Getting local ip")?;
	
	info!("Opening port using gateway at {:?}", gateway.addr);
	
	gateway.add_port(
		PortMappingProtocol::UDP,
		port,
		SocketAddr::new(local_ip, port),
		LEASE_DURATION,
		"FactorioCacher"
	).context("Adding port mapping")?;
	
	Ok(UpnpPortMapping {
		gateway,
		port,
	})
}

impl Drop for UpnpPortMapping {
	fn drop(&mut self) {
		info!("Removing port mapping");
		
		if let Err(err) = self.gateway.remove_port(PortMappingProtocol::UDP, self.port) {
			error!("Failed to remove port mapping: {}", err);
		}
	}
}

