use std::io::Read;

use fastcdc::v2020::StreamCDC;
use zeroize::Zeroizing;

pub const CDC_MIN_SIZE: usize = 1024 * 1024;
pub const CDC_AVG_SIZE: usize = 4 * 1024 * 1024;
pub const CDC_MAX_SIZE: usize = 8 * 1024 * 1024;
pub const CDC_SEED: u64 = 0;
/// Repository v1 fixes the crate, algorithm, normalization, sizes and seed.
pub const CDC_PROFILE: &str = "fastcdc-5.0.0/v2020/level1/1048576/4194304/8388608/seed0";

/// One owned plaintext chunk. Consumers must not accumulate chunks in memory.
pub struct Chunk {
    pub offset: u64,
    pub data: Zeroizing<Vec<u8>>,
}

/// The library retains at most max_size bytes plus one emitted chunk.
/// Dropping the iterator stops further reads; a worker must check cancellation
/// before requesting another chunk and join its blocking scanner on shutdown.
pub struct ChunkReader<R: Read> {
    pub(super) inner: StreamCDC<R>,
    pub(super) failed: bool,
}
