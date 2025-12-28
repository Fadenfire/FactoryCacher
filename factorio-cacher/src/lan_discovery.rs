use log::info;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use tokio::net::UdpSocket;

pub async fn lan_discovery_task(factorio_address_cell: Arc<Mutex<SocketAddr>>) {
	let listen_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 34196);
	let sock = UdpSocket::bind(listen_addr).await.expect("Couldn't bind LAN discovery socket");
	
	let mut buf = [0; 1500];
	
	let mut current_factorio_address = *factorio_address_cell.lock().unwrap();
	
	loop {
		let (len, addr) = sock.recv_from(&mut buf).await.unwrap();
		
		if len == 3 {
			let lan_port = u16::from_le_bytes([buf[1], buf[2]]);
			let lan_addr = SocketAddr::new(addr.ip(), lan_port);
			
			if current_factorio_address != lan_addr {
				current_factorio_address = lan_addr;
				*factorio_address_cell.lock().unwrap() = lan_addr;
				
				info!("LAN discovery updated factorio address to {}", lan_addr);
			}
		}
	}
}
