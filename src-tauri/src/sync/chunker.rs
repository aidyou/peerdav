use std::io::{self, Read};

use fastcdc::v2020::{Normalization, StreamCDC};
use zeroize::Zeroizing;

use super::types::{CDC_AVG_SIZE, CDC_MAX_SIZE, CDC_MIN_SIZE, CDC_SEED, Chunk, ChunkReader};

/// Reads incrementally using one continuous CDC state, independent of IO sizes.
pub fn chunk_reader<R: Read>(reader: R) -> ChunkReader<R> {
    ChunkReader {
        inner: StreamCDC::with_level_and_seed(
            reader,
            CDC_MIN_SIZE,
            CDC_AVG_SIZE,
            CDC_MAX_SIZE,
            Normalization::Level1,
            CDC_SEED,
        ),
        failed: false,
    }
}

impl<R: Read> Iterator for ChunkReader<R> {
    type Item = Result<Chunk, io::Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        self.inner.next().map(|result| match result {
            Ok(chunk) => Ok(Chunk {
                offset: chunk.offset,
                data: Zeroizing::new(chunk.data),
            }),
            Err(source) => {
                self.failed = true;
                Err(io::Error::other(source))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::io::Cursor;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn fixture(size: usize) -> Vec<u8> {
        let mut state = 0x6a09e667f3bcc909_u64;
        (0..size)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 32) as u8
            })
            .collect()
    }

    struct ShortReads<R> {
        inner: R,
        limit: usize,
    }
    impl<R: Read> Read for ShortReads<R> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let count = buffer.len().min(self.limit);
            self.inner.read(&mut buffer[..count])
        }
    }

    fn signatures(reader: impl Read) -> Vec<(u64, usize, [u8; 32])> {
        chunk_reader(reader)
            .map(|result| {
                let chunk = result.expect("read chunk");
                (
                    chunk.offset,
                    chunk.data.len(),
                    Sha256::digest(&chunk.data).into(),
                )
            })
            .collect()
    }

    #[test]
    fn boundaries_do_not_depend_on_read_buffer() {
        let data = fixture(24 * 1024 * 1024 + 17);
        let expected = signatures(Cursor::new(&data));
        for limit in [1, 4093, 65536] {
            assert_eq!(
                signatures(ShortReads {
                    inner: Cursor::new(&data),
                    limit
                }),
                expected
            );
        }
        assert_eq!(
            expected.iter().map(|(_, len, _)| len).sum::<usize>(),
            data.len()
        );
        assert!(expected.iter().all(|(_, len, _)| *len <= CDC_MAX_SIZE));
        assert_eq!(
            expected.iter().map(|(_, len, _)| *len).collect::<Vec<_>>(),
            [5_570_659, 8_161_808, 4_668_289, 4_913_394, 1_851_691],
        );
    }

    #[test]
    fn empty_and_small_files_are_not_padded() {
        assert!(chunk_reader(io::empty()).next().is_none());
        let chunks = signatures(Cursor::new(b"small"));
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].0, 0);
        assert_eq!(chunks[0].1, 5);
    }

    #[test]
    fn local_insert_delete_and_overwrite_reuse_existing_chunks() {
        let original = fixture(64 * 1024 * 1024);
        let known: BTreeMap<_, _> = signatures(Cursor::new(&original))
            .into_iter()
            .map(|(_, len, digest)| (digest, len))
            .collect();
        let middle = original.len() / 2;
        let mut inserted = original.clone();
        inserted.splice(middle..middle, vec![0x41; 4096]);
        let mut deleted = original.clone();
        deleted.drain(middle..middle + 4096);
        let mut overwritten = original.clone();
        overwritten[middle..middle + 4096].fill(0x42);
        for data in [&original, &inserted, &deleted, &overwritten] {
            let mut uploaded = 0;
            let mut reused = 0;
            for (_, len, digest) in signatures(Cursor::new(data)) {
                if known.get(&digest) == Some(&len) {
                    reused += len;
                } else {
                    uploaded += len;
                }
            }
            if std::ptr::eq(data, &original) {
                assert_eq!(uploaded, 0);
            }
            assert!(reused >= original.len() * 75 / 100, "reused {reused}");
            assert!(uploaded < original.len() / 4, "uploaded {uploaded}");
        }
    }

    #[test]
    fn reader_is_lazy_and_memory_window_does_not_scale_with_file_length() {
        struct Counted {
            remaining: u64,
            reads: Arc<AtomicUsize>,
        }
        impl Read for Counted {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let count = self.remaining.min(buffer.len() as u64) as usize;
                buffer[..count].fill(0x31);
                self.remaining -= count as u64;
                self.reads.fetch_add(count, Ordering::SeqCst);
                Ok(count)
            }
        }
        for size in [64 * 1024 * 1024_u64, 4 * 1024 * 1024 * 1024_u64] {
            let reads = Arc::new(AtomicUsize::new(0));
            let mut reader = chunk_reader(Counted {
                remaining: size,
                reads: reads.clone(),
            });
            assert_eq!(reads.load(Ordering::SeqCst), 0);
            let chunk = reader.next().expect("first chunk").expect("read");
            assert!(chunk.data.len() <= CDC_MAX_SIZE);
            assert!(reads.load(Ordering::SeqCst) <= CDC_MAX_SIZE);
            drop(chunk);
            for _ in 0..3 {
                let chunk = reader.next().expect("chunk").expect("read");
                assert!(chunk.data.len() <= CDC_MAX_SIZE);
                drop(chunk);
            }
        }
    }

    #[test]
    fn read_error_is_terminal_and_preserves_source() {
        struct Failed;
        impl Read for Failed {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "scan-marker",
                ))
            }
        }
        let mut reader = chunk_reader(Failed);
        let error = reader.next().expect("error").err().expect("read failure");
        assert!(error.to_string().contains("scan-marker"));
        assert!(reader.next().is_none());
    }
}
