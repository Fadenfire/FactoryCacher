use std::path::PathBuf;

#[derive(Debug)]
pub struct CacheOptions {
	pub cache_path: Option<PathBuf>,
	pub size_limit: u64,
	pub save_interval: u64,
	pub compression_level: i32,
}

#[derive(Debug)]
pub struct TransportOptions {
	pub compression_level: i32,
}

#[macro_export]
macro_rules! client_args {
    ($($fields:tt)*) => {
		#[derive(FromArgs)]
		/// Run the client
		#[argh(subcommand, name = "client")]
		struct ClientArgs {
			$($fields)*
			
			// Cache options
			
			#[argh(option, short = 'c')]
			/// location of cache file, defaults to 'persistent-cache' in the CWD
			cache_path: Option<PathBuf>,
			
			#[argh(option, default = "500_000_000")]
			/// max size of the chunk cache, defaults to 500MB
			cache_limit: u64,
			
			#[argh(option, default = "60")]
			/// how often to try to save the cache in seconds, defaults to 60s
			cache_interval: u64,
			
			#[argh(option, default = "3")]
			/// zstd compression level used for the on-disk chunk cache, must be between 1-22, defaults to 3
			cache_compression: i32,
			
			// Transport options
			
			#[argh(option, default = "common::protocol_utils::DEFAULT_ZSTD_COMPRESSION_LEVEL")]
			/// zstd compression level used messages send over the network, must be between 1-22, defaults to 11
			net_compression: i32,
		}
		
		impl ClientArgs {
			pub fn cache_options(&self) -> common::cli_args::CacheOptions {
				common::cli_args::CacheOptions {
					cache_path: self.cache_path.clone(),
					size_limit: self.cache_limit,
					save_interval: self.cache_interval,
					compression_level: self.cache_compression,
				}
			}
			
			pub fn transport_options(&self) -> common::cli_args::TransportOptions {
				common::cli_args::TransportOptions {
					compression_level: self.net_compression,
				}
			}
		}
	};
}

pub use client_args;

#[macro_export]
macro_rules! server_args {
    ($($fields:tt)*) => {
		#[derive(FromArgs)]
		/// Run the server
		#[argh(subcommand, name = "server")]
		struct ServerArgs {
			$($fields)*
			
			// Transport options
			
			#[argh(option, default = "common::protocol_utils::DEFAULT_ZSTD_COMPRESSION_LEVEL")]
			/// zstd compression level used messages send over the network, must be between 1-22, defaults to 11
			net_compression: i32,
		}
		
		impl ServerArgs {
			pub fn transport_options(&self) -> common::cli_args::TransportOptions {
				common::cli_args::TransportOptions {
					compression_level: self.net_compression,
				}
			}
		}
	};
}

pub use server_args;
