use std::path::PathBuf;

#[derive(Debug)]
pub struct CacheOptions {
	pub cache_path: Option<PathBuf>,
	pub cache_limit: u64,
	pub cache_save_interval: u64,
}

#[macro_export]
macro_rules! client_args {
    ($($fields:tt)*) => {
		#[derive(FromArgs)]
		/// Run the client
		#[argh(subcommand, name = "client")]
		struct ClientArgs {
			$($fields)*
			
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
		
		impl ClientArgs {
			pub fn cache_options(&self) -> common::cli_args::CacheOptions {
				common::cli_args::CacheOptions {
					cache_path: self.cache_path.clone(),
					cache_limit: self.cache_limit,
					cache_save_interval: self.cache_save_interval,
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
		}
		
		impl ServerArgs {
		
		}
	};
}

pub use server_args;
