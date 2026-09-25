//! The outer ABF framing: an uncompressed `STREAM_STORAGE` backup, or one
//! compressed with single-threaded or multithreaded XPress9.
//!
//! Every variant is decoded into a writer as a stream, so a large model never
//! has to fit in memory. Sizes are validated before anything is allocated.

use std::io::{self, Read, Write};

use ripbi_xpress9::Decoder;

use crate::{Error, Result};

/// Length of the UTF-16 signature text that opens a compressed backup.
pub(super) const COMPRESSED_HEADER_LEN: usize = 102;
/// Length of the UTF-16 `STREAM_STORAGE` signature, BOM included.
pub(super) const STREAM_STORAGE_LEN: usize = 72;

const SINGLE_THREADED: &str = "This backup was created using XPress9 compression.";
const MULTITHREADED: &str = "This backup was created using multithreaded XPrs9.";
const STREAM_STORAGE: &str = "STREAM_STORAGE_SIGNATURE_)!@#$%^&*(";

/// No real chunk comes near this; a larger header is corruption, and
/// rejecting it keeps a hostile file from forcing a huge allocation.
const MAX_CHUNK: usize = 64 * 1024 * 1024;

/// Which framing a backup uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Framing {
    StreamStorage,
    SingleThreaded,
    Multithreaded,
}

/// Recognizes a backup from its first [`COMPRESSED_HEADER_LEN`] bytes.
pub(super) fn framing(header: &[u8]) -> Option<Framing> {
    if header.starts_with(&utf16_bom(STREAM_STORAGE)) {
        return Some(Framing::StreamStorage);
    }
    if header.starts_with(&utf16(SINGLE_THREADED)) {
        return Some(Framing::SingleThreaded);
    }
    if header.starts_with(&utf16(MULTITHREADED)) {
        return Some(Framing::Multithreaded);
    }
    None
}

/// The signature a decoded stream must open with.
pub(super) fn stream_storage_signature() -> Vec<u8> {
    utf16_bom(STREAM_STORAGE)
}

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn utf16_bom(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xfe];
    bytes.extend(utf16(text));
    bytes
}

/// Decodes a whole backup from `reader` into `out`, returning the decoded
/// length.
pub(super) fn decode(mut reader: impl Read, out: &mut impl Write) -> Result<u64> {
    let mut header = [0u8; COMPRESSED_HEADER_LEN];
    let read = read_full(&mut reader, &mut header)?;
    let framing = framing(&header[..read])
        .ok_or_else(|| corrupt("the stream has no recognized ABF signature"))?;
    match framing {
        Framing::StreamStorage => {
            out.write_all(&header[..read])?;
            Ok(read as u64 + io::copy(&mut reader, out)?)
        }
        Framing::SingleThreaded => {
            let mut decoder = new_decoder()?;
            let mut total = 0;
            while let Some(size) = read_chunk_header(&mut reader)? {
                total += decode_chunk(&mut reader, &mut decoder, size, MAX_CHUNK, out)?;
            }
            Ok(total)
        }
        Framing::Multithreaded => {
            let mut fields = [0u64; 5];
            for field in &mut fields {
                let mut bytes = [0u8; 8];
                if read_full(&mut reader, &mut bytes)? != bytes.len() {
                    return Err(corrupt("the multithreaded header is truncated"));
                }
                *field = u64::from_le_bytes(bytes);
            }
            let [
                main_chunks,
                prefix_chunks,
                prefix_threads,
                main_threads,
                chunk_size,
            ] = fields;
            let limit = usize::try_from(chunk_size)
                .ok()
                .filter(|size| (1..=MAX_CHUNK).contains(size))
                .ok_or_else(|| corrupt(format!("chunk size {chunk_size} is out of range")))?;
            let mut total = 0;
            for (threads, chunks) in [(prefix_threads, prefix_chunks), (main_threads, main_chunks)]
            {
                // Counts are bounded by the input itself: every chunk needs at
                // least a header, so a lying count fails at end of input.
                for _ in 0..threads {
                    let mut decoder = new_decoder()?;
                    for _ in 0..chunks {
                        let size = read_chunk_header(&mut reader)?.ok_or_else(|| {
                            corrupt("a chunk group ends before its declared count")
                        })?;
                        total += decode_chunk(&mut reader, &mut decoder, size, limit, out)?;
                    }
                }
            }
            Ok(total)
        }
    }
}

fn new_decoder() -> Result<Decoder> {
    Decoder::new().map_err(|error| corrupt(error.to_string()))
}

/// Declared sizes of one chunk: `(uncompressed, compressed)`.
type ChunkSize = (usize, usize);

/// Reads the next chunk header, or `None` at a clean end of input.
fn read_chunk_header(reader: &mut impl Read) -> Result<Option<ChunkSize>> {
    let mut bytes = [0u8; 8];
    match read_full(reader, &mut bytes)? {
        0 => Ok(None),
        8 => {
            let [a, b, c, d, e, f, g, h] = bytes;
            Ok(Some((
                u32::from_le_bytes([a, b, c, d]) as usize,
                u32::from_le_bytes([e, f, g, h]) as usize,
            )))
        }
        _ => Err(corrupt("a chunk header is truncated")),
    }
}

fn decode_chunk(
    reader: &mut impl Read,
    decoder: &mut Decoder,
    (uncompressed, compressed): ChunkSize,
    limit: usize,
    out: &mut impl Write,
) -> Result<u64> {
    if uncompressed == 0 || compressed == 0 || uncompressed > limit || compressed > MAX_CHUNK {
        return Err(corrupt(format!(
            "chunk sizes {uncompressed}/{compressed} are out of range"
        )));
    }
    // `take` + `read_to_end` grows with the bytes actually present, so a
    // chunk that claims more than the input holds cannot force the full
    // allocation before it is found truncated.
    let mut input = Vec::new();
    reader.take(compressed as u64).read_to_end(&mut input)?;
    if input.len() != compressed {
        return Err(corrupt("a compressed chunk is truncated"));
    }
    let mut buffer = vec![0; uncompressed];
    decoder
        .decode_chunk(&input, &mut buffer)
        .map_err(|error| corrupt(error.to_string()))?;
    out.write_all(&buffer)?;
    Ok(uncompressed as u64)
}

/// Fills `buf` as far as the input allows, returning how much was read.
fn read_full(reader: &mut impl Read, buf: &mut [u8]) -> Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(filled)
}

pub(super) fn corrupt(detail: impl Into<String>) -> Error {
    Error::DataModel(detail.into())
}
