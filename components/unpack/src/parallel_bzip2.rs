//! bzip2 decoded on every core. pbzip2, which Yocto compresses images with,
//! cuts its input in 900 kB pieces and compresses each into a bzip2 stream
//! of its own, one after the other in the file: the streams decode
//! independently, a batch of them at a time, one per core, and are handed
//! out in order. A file of a single stream (plain `bzip2`) has nothing to
//! cut: it decodes on one thread, streaming.

use std::{
    io::{self, Read},
    sync::mpsc::{self, Receiver, SyncSender},
    thread::{self, JoinHandle},
};

use bzip2::read::MultiBzDecoder;

/// A stream's start: "BZh", its block size digit, then its first block's
/// magic (the BCD digits of pi), all byte-aligned at a stream's start.
const BLOCK_MAGIC: [u8; 6] = [0x31, 0x41, 0x59, 0x26, 0x53, 0x59];
const HEADER_LEN: usize = 4 + BLOCK_MAGIC.len();
/// Compressed input read at a time.
const READ_SIZE: usize = 1 << 20;
/// pbzip2 streams are a little under 1 MB at worst: a piece this big with
/// no stream start in it is a single-stream file.
const MAX_PIECE: usize = 16 << 20;
/// Decoded pieces waiting for the reader, per decoding thread: bounds memory
/// at a few MB per core.
const QUEUE_PER_THREAD: usize = 2;
/// Most pieces merged back into one when a stream start found in the data
/// wasn't one (a match inside a stream's compressed bits).
const MAX_MERGE: usize = 4;

type Piece = io::Result<Vec<u8>>;

pub struct ParallelBzDecoder {
    pieces: Receiver<Piece>,
    worker: Option<JoinHandle<()>>,
    current: Vec<u8>,
    offset: usize,
}

impl ParallelBzDecoder {
    pub fn new<R: Read + Send + 'static>(input: R) -> Self {
        let threads = thread::available_parallelism().map_or(1, |n| n.get());
        Self::with(input, threads, MAX_PIECE)
    }

    fn with<R: Read + Send + 'static>(input: R, threads: usize, max_piece: usize) -> Self {
        let (tx, pieces) = mpsc::sync_channel(threads * QUEUE_PER_THREAD);
        let worker = thread::spawn(move || {
            if let Err(e) = decode(input, threads, max_piece, &tx) {
                let _ = tx.send(Err(e));
            }
        });
        Self {
            pieces,
            worker: Some(worker),
            current: Vec::new(),
            offset: 0,
        }
    }
}

impl Read for ParallelBzDecoder {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.offset == self.current.len() {
            match self.pieces.recv() {
                Ok(piece) => {
                    self.current = piece?;
                    self.offset = 0;
                }
                // The decoding thread is gone: the end of the image, unless
                // it died, which must not pass for one.
                Err(_) => {
                    if let Some(worker) = self.worker.take() {
                        if worker.join().is_err() {
                            return Err(io::Error::other("bzip2 decoding thread panicked"));
                        }
                    }
                    return Ok(0);
                }
            }
        }
        let n = buf.len().min(self.current.len() - self.offset);
        buf[..n].copy_from_slice(&self.current[self.offset..self.offset + n]);
        self.offset += n;
        Ok(n)
    }
}

/// Cut `input` into pieces at stream starts and decode them a batch at a
/// time, until its end, or until the reader is gone.
fn decode<R: Read + Send + 'static>(
    mut input: R,
    threads: usize,
    max_piece: usize,
    tx: &SyncSender<Piece>,
) -> io::Result<()> {
    let mut batch = Vec::with_capacity(threads);
    let mut pending = None;
    let mut current = Vec::new();
    let mut buf = vec![0; READ_SIZE];
    loop {
        let n = match input.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        // Only the tail that could hold a start straddling the reads is
        // searched again; never offset 0, where `current` itself starts.
        let mut from = current.len().saturating_sub(HEADER_LEN - 1).max(1);
        current.extend_from_slice(&buf[..n]);
        while let Some(at) = find_stream_start(&current, from) {
            let rest = current.split_off(at);
            batch.push(std::mem::replace(&mut current, rest));
            from = 1;
            if batch.len() == threads && !flush(&mut batch, &mut pending, tx)? {
                return Ok(());
            }
        }
        if current.len() > max_piece {
            if !flush(&mut batch, &mut pending, tx)? {
                return Ok(());
            }
            let (merged, _) = pending.unwrap_or_default();
            let rest = io::Cursor::new(merged)
                .chain(io::Cursor::new(current))
                .chain(input);
            return stream(MultiBzDecoder::new(rest), tx);
        }
    }
    if !current.is_empty() {
        batch.push(current);
    }
    flush(&mut batch, &mut pending, tx)?;
    match pending {
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "corrupt bzip2 data",
        )),
        None => Ok(()),
    }
}

/// The first stream start in `data` at or after `from`.
fn find_stream_start(data: &[u8], from: usize) -> Option<usize> {
    data.get(from..)?
        .windows(HEADER_LEN)
        .position(|w| {
            w.starts_with(b"BZh") && (b'1'..=b'9').contains(&w[3]) && w[4..] == BLOCK_MAGIC
        })
        .map(|at| from + at)
}

/// Decode `batch`, one thread per piece, and send the results in order.
/// A piece that fails is held in `pending` and merged with the next ones
/// until they decode together: what a stream start that wasn't one cut in
/// two. `false` when the reader is gone.
fn flush(
    batch: &mut Vec<Vec<u8>>,
    pending: &mut Option<(Vec<u8>, usize)>,
    tx: &SyncSender<Piece>,
) -> io::Result<bool> {
    let decoded: Vec<Piece> = thread::scope(|s| {
        let workers: Vec<_> = batch
            .iter()
            .map(|piece| s.spawn(move || decode_piece(piece)))
            .collect();
        workers
            .into_iter()
            .map(|w| {
                w.join()
                    .unwrap_or_else(|_| Err(io::Error::other("bzip2 decoding thread panicked")))
            })
            .collect()
    });
    for (raw, result) in batch.drain(..).zip(decoded) {
        let out = match (pending.take(), result) {
            (None, Ok(out)) => out,
            (None, Err(_)) => {
                *pending = Some((raw, 1));
                continue;
            }
            (Some((mut merged, count)), _) => {
                merged.extend_from_slice(&raw);
                match decode_piece(&merged) {
                    Ok(out) => out,
                    Err(e) if count + 1 >= MAX_MERGE => return Err(e),
                    Err(_) => {
                        *pending = Some((merged, count + 1));
                        continue;
                    }
                }
            }
        };
        if tx.send(Ok(out)).is_err() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn decode_piece(piece: &[u8]) -> Piece {
    let mut out = Vec::with_capacity(1 << 20);
    MultiBzDecoder::new(piece).read_to_end(&mut out)?;
    Ok(out)
}

/// Decode a single-stream rest on this thread, a chunk at a time.
fn stream(mut decoder: impl Read, tx: &SyncSender<Piece>) -> io::Result<()> {
    loop {
        let mut chunk = vec![0; READ_SIZE];
        let n = match decoder.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        chunk.truncate(n);
        if tx.send(Ok(chunk)).is_err() {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    /// Recognisable, and not compressible to nothing.
    fn data(len: usize) -> Vec<u8> {
        let mut x = 0x2545_f491_u32;
        (0..len)
            .map(|i| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                if i % 7 == 0 {
                    0
                } else {
                    x as u8
                }
            })
            .collect()
    }

    fn bzip2(data: &[u8]) -> Vec<u8> {
        let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    /// As pbzip2 does: one stream per `piece` bytes of input.
    fn pbzip2(data: &[u8], piece: usize) -> Vec<u8> {
        data.chunks(piece).flat_map(bzip2).collect()
    }

    fn read_all(mut decoder: ParallelBzDecoder) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        decoder.read_to_end(&mut out)?;
        Ok(out)
    }

    #[test]
    fn a_multi_stream_file_decodes_in_order() {
        let input = data(3 << 20);
        let compressed = pbzip2(&input, 100_000);

        let decoded = read_all(ParallelBzDecoder::with(
            io::Cursor::new(compressed),
            4,
            MAX_PIECE,
        ));
        assert_eq!(decoded.unwrap(), input);
    }

    #[test]
    fn a_single_stream_file_falls_back_to_streaming() {
        let input = data(2 << 20);
        let compressed = bzip2(&input);
        // Smaller than the stream, so it can't be held as one piece.
        let max_piece = compressed.len() / 4;

        let decoded = read_all(ParallelBzDecoder::with(
            io::Cursor::new(compressed),
            4,
            max_piece,
        ));
        assert_eq!(decoded.unwrap(), input);
    }

    #[test]
    fn a_stream_cut_in_two_is_merged_back() {
        let input = data(300_000);
        let stream = bzip2(&input);
        let mut batch = vec![
            stream[..stream.len() / 2].to_vec(),
            stream[stream.len() / 2..].to_vec(),
        ];
        let (tx, rx) = mpsc::sync_channel(4);
        let mut pending = None;

        assert!(flush(&mut batch, &mut pending, &tx).unwrap());
        assert!(pending.is_none());
        drop(tx);
        let decoded: Vec<u8> = rx.into_iter().flat_map(Result::unwrap).collect();
        assert_eq!(decoded, input);
    }

    #[test]
    fn corrupt_data_is_an_error_not_an_early_end() {
        let input = data(1 << 20);
        let mut compressed = pbzip2(&input, 100_000);
        let middle = compressed.len() / 2;
        compressed[middle..middle + 64].fill(0x55);

        let decoded = read_all(ParallelBzDecoder::with(
            io::Cursor::new(compressed),
            4,
            MAX_PIECE,
        ));
        assert!(decoded.is_err());
    }
}
