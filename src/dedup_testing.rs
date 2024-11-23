use crate::chunker::RabinKarpHash;
use hashlink::LinkedHashMap;
use std::io::Read;
use std::path::PathBuf;

pub fn dedup_test() {
	let arg1 = PathBuf::from(std::env::args().nth(1).unwrap());
	let arg2 = PathBuf::from(std::env::args().nth(2).unwrap());
	
	let world1_size = std::fs::metadata(&arg1).unwrap().len();
	let (world1_chunks, _world1_size_bet) = chunk_zip(arg1).unwrap();
	
	let world2_size = std::fs::metadata(&arg2).unwrap().len();
	let (world2_chunks, _world2_size_bet) = chunk_zip(arg2).unwrap();
	
	let transform_chunks = world2_chunks.values()
		.filter(|chunk| !world1_chunks.contains_key(&chunk.hash))
		.collect::<Vec<_>>();
	//.map(|chunk| chunk.compressed_size);
	
	let mut transform_size = 0usize;
	
	for batch in transform_chunks.chunks(512) {
		let mut batch_data = Vec::new();
		
		for chunk in batch {
			batch_data.extend_from_slice(&chunk.data);
			transform_size += 32;
		}
		
		let com_data = zstd::encode_all(&batch_data[..], 10).unwrap();
		transform_size += com_data.len();
		
		println!("Batch size: {}", batch_data.len());
	}
	
	// let transform_data = transform_iter.clone()
	// 	.fold(Vec::new(), |mut buf, chunk| { buf.extend_from_slice(&chunk.data); buf });
	// 
	// let com_trans_data = zstd::encode_all(&transform_data[..], 10).unwrap();
	// let com_trans_data = miniz_oxide::deflate::compress_to_vec(&transform_data, 9);
	
	// let transform_size: usize = com_trans_data.len() + transform_iter.count() * 32;
	
	let world1_chunks_size: usize = world1_chunks.values()
		.map(|chunk| chunk.compressed_size)
		.sum();
	
	let world2_chunks_size: usize = world2_chunks.values()
		.map(|chunk| chunk.compressed_size)
		.sum();
	
	let mut all_chunks = world1_chunks.clone();
	all_chunks.extend(world2_chunks);
	
	let total_dedup_size: usize = all_chunks.values()
		.map(|chunk| chunk.compressed_size)
		.sum();
	
	println!("World 1 size: {:.2} MB", world1_size as f64 / 1_000_000.0);
	println!("World 1 dedup size: {:.2} MB", world1_chunks_size as f64 / 1_000_000.0);
	println!("World 2 size: {:.2} MB", world2_size as f64 / 1_000_000.0);
	println!("World 2 dedup size: {:.2} MB", world2_chunks_size as f64 / 1_000_000.0);
	// println!("World 1 size bet: {:.2} MB", world1_size_bet as f64 / 1_000_000.0);
	// println!("World 2 size bet: {:.2} MB", world2_size_bet as f64 / 1_000_000.0);
	println!();
	
	println!("Chunk count: {}", all_chunks.len());
	println!("Deduped size: {:.2} MB", total_dedup_size as f64 / 1_000_000.0);
	println!("Original size: {:.2} MB", (world1_size + world2_size) as f64 / 1_000_000.0);
	println!("World 2 -> World 2: {:.2} MB", transform_size as f64 / 1_000_000.0);
}

fn chunk_zip(zip_path: PathBuf) -> anyhow::Result<(LinkedHashMap<blake3::Hash, Chunk>, usize)> {
	let zip_file = std::fs::File::open(&zip_path)?;
	let mut archive = zip::ZipArchive::new(zip_file)?;
	
	let mut better_size = 0usize;
	let mut chunk_table = LinkedHashMap::new();
	
	let mut buffer = Vec::new();
	
	for i in 0..archive.len() {
		let mut file = archive.by_index(i)?;
		
		// println!("{:?}", file.name());
		
		buffer.clear();
		file.read_to_end(&mut buffer)?;
		
		let is_dat = file.name().rsplit_once('/').unwrap().1
			.strip_prefix("level.dat")
			.is_some_and(|s| s.chars().all(char::is_numeric));
		
		if is_dat {
			let decom_data = miniz_oxide::inflate::decompress_to_vec(&buffer[2..]).unwrap();
			
			chunk_file(&decom_data, &mut chunk_table);
			
			// better_size += miniz_oxide::deflate::compress_to_vec(&decom_data, 9).len();
		} else {
			chunk_file(&buffer, &mut chunk_table);
			
			// better_size += miniz_oxide::deflate::compress_to_vec(&buffer, 9).len();
		}
	}
	
	Ok((chunk_table, better_size))
}

#[derive(Clone)]
struct Chunk {
	hash: blake3::Hash,
	data: Vec<u8>,
	// uncompressed_size: usize,
	compressed_size: usize,
}

impl Chunk {
	pub fn new(data: &[u8]) -> Self {
		let hash = blake3::hash(&data);
		let compressed_data = miniz_oxide::deflate::compress_to_vec(data, 6);
		// let compressed_data = zstd::encode_all(&data[..], 0).unwrap();
		
		Self {
			hash,
			data: data.to_vec(),
			// uncompressed_size: data.len(),
			compressed_size: compressed_data.len(),
		}
	}
}

fn chunk_file(file_data: &[u8], chunk_table: &mut LinkedHashMap<blake3::Hash, Chunk>) {
	let mut current_chunk: Vec<u8> = Vec::new();
	let mut rolling_hash = RabinKarpHash::new();
	
	const MIN_SIZE: usize = 1 << 9;
	const MAX_SIZE: usize = 1 << 12;
	const AVG_SIZE: u32 = 1 << 10;
	
	for &byte in file_data {
		current_chunk.push(byte);
		
		if current_chunk.len() > MIN_SIZE {
			let hash = rolling_hash.update(byte);
			
			if (hash & (AVG_SIZE - 1)) == 0 || current_chunk.len() >= MAX_SIZE {
				let chunk = Chunk::new(&current_chunk);
				chunk_table.insert(chunk.hash, chunk);
				
				current_chunk.clear();
				rolling_hash.reset();
			}
		}
	}
	
	if !current_chunk.is_empty() {
		let chunk = Chunk::new(&current_chunk);
		chunk_table.insert(chunk.hash, chunk);
	}
}