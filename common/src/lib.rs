use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use log::info;
use quinn::Endpoint;
use tokio::select;
use crate::chunk_cache::ChunkCache;

pub mod dedup;
pub mod utils;
pub mod chunk_cache;
pub mod quic;
pub mod protocol_utils;

pub async fn cli_wrapper<F, Fut>(endpoint: &Endpoint, run: F)
where
	F: FnOnce() -> Fut,
	Fut: Future<Output = anyhow::Result<()>>,
{
	select! {
		result = run() => result.unwrap(),
		_ = tokio::signal::ctrl_c() => {}
	}
	
	endpoint.close(0u32.into(), b"quit");
	
	select! {
		_ = endpoint.wait_idle() => {},
		_ = tokio::signal::ctrl_c() => {}
	}
	
	info!("Shutdown");
}

pub async fn create_chunk_cache(
	cache_path: &Option<PathBuf>,
	cache_limit: u64,
	cache_save_interval: u64,
) -> anyhow::Result<Arc<ChunkCache>> {
	let cache_path = cache_path.clone()
		.unwrap_or_else(|| std::path::absolute("persistent-cache").unwrap());
	
	let chunk_cache;
	
	if cache_path.exists() {
		info!("Loading cache from {}", cache_path.display());
		
		let compressed_size = tokio::fs::metadata(&cache_path).await?.len();
		chunk_cache = Arc::new(ChunkCache::load_from_file(cache_limit, cache_path.clone()).await?);
		
		info!(
			"Loaded {} chunks ({}B, {}B compressed) from the cache",
			chunk_cache.len(),
			utils::abbreviate_number(chunk_cache.total_size()),
			utils::abbreviate_number(compressed_size)
		);
	} else {
		chunk_cache = Arc::new(ChunkCache::new(cache_limit));
	}
	
	info!("The cache has a limit of {}B", utils::abbreviate_number(cache_limit));
	
	chunk_cache.start_writer(cache_path, Duration::from_secs(cache_save_interval));
	
	Ok(chunk_cache)
}

pub fn setup_logging() {
	use simplelog::*;
	
	let config = ConfigBuilder::new()
		.set_time_format_custom(format_description!("[[[hour repr:12]:[minute]:[second] [period]]"))
		.set_time_offset_to_local().unwrap()
		.build();
	
	TermLogger::init(LevelFilter::Info, config, TerminalMode::Stdout, ColorChoice::Auto).expect("Unable to init logger");
}
