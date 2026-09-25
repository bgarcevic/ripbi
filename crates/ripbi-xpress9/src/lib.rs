//! Safe XPress9 chunk decoding for Power BI ABF backups.
//!
//! A `.pbix` `DataModel` member and a standalone `.abf` backup are compressed
//! with Microsoft's XPress9 codec. This crate compiles Microsoft's
//! MIT-licensed C implementation statically (see `NOTICE.md`) and exposes it
//! as a stateful [`Decoder`]: chunks of one stream share a history window, so
//! they must be fed to the same decoder in order.
//!
//! # The `unsafe` exception
//!
//! Every other ripbi crate carries `#![forbid(unsafe_code)]`. This crate is the
//! one approved exception: it holds all of ripbi's own FFI, confined to the
//! handful of `unsafe` blocks below, each with a `SAFETY` note. The C side
//! never prints and never aborts; every failure comes back as an [`Error`].

#![deny(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]

use std::ffi::{c_uint, c_void};
use std::ptr::NonNull;

unsafe extern "C" {
    fn ripbi_xpress9_decoder_new() -> *mut c_void;
    fn ripbi_xpress9_decoder_free(decoder: *mut c_void);
    fn ripbi_xpress9_decode(
        decoder: *mut c_void,
        input: *const u8,
        input_len: c_uint,
        output: *mut u8,
        output_len: c_uint,
        written: *mut c_uint,
    ) -> c_uint;
}

#[cfg(feature = "encoder")]
unsafe extern "C" {
    fn ripbi_xpress9_encoder_new() -> *mut c_void;
    fn ripbi_xpress9_encoder_free(encoder: *mut c_void);
    fn ripbi_xpress9_encode(
        encoder: *mut c_void,
        input: *const u8,
        input_len: c_uint,
        output: *mut u8,
        output_len: c_uint,
        written: *mut c_uint,
    ) -> c_uint;
}

/// The shim's code for "the chunk decodes to more bytes than declared".
const OUTPUT_OVERFLOW: c_uint = 1000;

/// Why a chunk could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The codec could not allocate its state.
    OutOfMemory,
    /// A buffer is larger than the codec's 32-bit length fields allow.
    TooLarge,
    /// The codec rejected the input; the value is its `Xpress9Status_*` code.
    Corrupt(u32),
    /// The chunk decoded to a different size than its header declared.
    SizeMismatch {
        /// The declared size.
        expected: usize,
        /// The size actually produced (a lower bound when larger).
        actual: usize,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfMemory => f.write_str("XPress9 codec could not allocate its state"),
            Self::TooLarge => f.write_str("XPress9 buffer exceeds 4 GiB"),
            Self::Corrupt(code) => write!(f, "XPress9 data is corrupt (status {code})"),
            Self::SizeMismatch { expected, actual } => write!(
                f,
                "XPress9 chunk decoded to {actual} bytes, header declared {expected}"
            ),
        }
    }
}

impl std::error::Error for Error {}

/// A stateful XPress9 decoder: one per independently compressed stream.
#[derive(Debug)]
pub struct Decoder {
    raw: NonNull<c_void>,
}

// SAFETY: the decoder state is plain heap memory owned exclusively by this
// handle; nothing in it is tied to the creating thread. `&mut self` on every
// call rules out concurrent use, so it is `Send` but deliberately not `Sync`.
unsafe impl Send for Decoder {}

impl Decoder {
    /// Creates a decoder with an empty history window.
    ///
    /// # Errors
    /// [`Error::OutOfMemory`] when the codec cannot allocate its window.
    pub fn new() -> Result<Self, Error> {
        // SAFETY: no arguments; the shim returns either NULL or a decoder
        // that only `ripbi_xpress9_decoder_free` releases (in `Drop`).
        let raw = unsafe { ripbi_xpress9_decoder_new() };
        NonNull::new(raw)
            .map(|raw| Self { raw })
            .ok_or(Error::OutOfMemory)
    }

    /// Decodes one chunk into `out`, which must be exactly the chunk's
    /// declared uncompressed size. The history window carries over to the
    /// next call.
    ///
    /// # Errors
    /// When the input is corrupt or decodes to any size other than
    /// `out.len()`. The decoder's history is then undefined; drop it.
    pub fn decode_chunk(&mut self, compressed: &[u8], out: &mut [u8]) -> Result<(), Error> {
        let input_len = c_uint::try_from(compressed.len()).map_err(|_| Error::TooLarge)?;
        let output_len = c_uint::try_from(out.len()).map_err(|_| Error::TooLarge)?;
        let mut written: c_uint = 0;
        // SAFETY: `self.raw` is a live decoder (created in `new`, freed only
        // in `Drop`, used exclusively through `&mut self`). The input pointer
        // is valid for `input_len` reads and the output pointer for
        // `output_len` writes, both borrowed for the whole call; the shim
        // never writes past `output_len` and keeps no pointer after returning
        // (it detaches the input before it does).
        let status = unsafe {
            ripbi_xpress9_decode(
                self.raw.as_ptr(),
                compressed.as_ptr(),
                input_len,
                out.as_mut_ptr(),
                output_len,
                &mut written,
            )
        };
        let actual = written as usize;
        match status {
            0 if actual == out.len() => Ok(()),
            0 => Err(Error::SizeMismatch {
                expected: out.len(),
                actual,
            }),
            OUTPUT_OVERFLOW => Err(Error::SizeMismatch {
                expected: out.len(),
                actual: actual + 1,
            }),
            code => Err(Error::Corrupt(code)),
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: `self.raw` came from `ripbi_xpress9_decoder_new` and is
        // released exactly once, here.
        unsafe { ripbi_xpress9_decoder_free(self.raw.as_ptr()) }
    }
}

/// A stateful XPress9 encoder, compiled only with the `encoder` feature so
/// tests can build ABF fixtures.
#[cfg(feature = "encoder")]
#[derive(Debug)]
pub struct Encoder {
    raw: NonNull<c_void>,
}

// SAFETY: as for `Decoder`.
#[cfg(feature = "encoder")]
unsafe impl Send for Encoder {}

#[cfg(feature = "encoder")]
impl Encoder {
    /// Creates an encoder with an empty history window.
    ///
    /// # Errors
    /// [`Error::OutOfMemory`] when the codec cannot allocate its window.
    pub fn new() -> Result<Self, Error> {
        // SAFETY: as in `Decoder::new`.
        let raw = unsafe { ripbi_xpress9_encoder_new() };
        NonNull::new(raw)
            .map(|raw| Self { raw })
            .ok_or(Error::OutOfMemory)
    }

    /// Encodes one chunk that a [`Decoder`] fed the same chunk sequence
    /// decodes back to `original`.
    ///
    /// # Errors
    /// When the codec fails.
    pub fn encode_chunk(&mut self, original: &[u8]) -> Result<Vec<u8>, Error> {
        let input_len = c_uint::try_from(original.len()).map_err(|_| Error::TooLarge)?;
        let mut out = vec![0; original.len() * 2 + 1024];
        let output_len = c_uint::try_from(out.len()).map_err(|_| Error::TooLarge)?;
        let mut written: c_uint = 0;
        // SAFETY: as in `Decoder::decode_chunk`, with the encoder handle.
        let status = unsafe {
            ripbi_xpress9_encode(
                self.raw.as_ptr(),
                original.as_ptr(),
                input_len,
                out.as_mut_ptr(),
                output_len,
                &mut written,
            )
        };
        if status != 0 {
            return Err(Error::Corrupt(status));
        }
        out.truncate(written as usize);
        Ok(out)
    }
}

#[cfg(feature = "encoder")]
impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: as in `Decoder::drop`.
        unsafe { ripbi_xpress9_encoder_free(self.raw.as_ptr()) }
    }
}

#[cfg(test)]
mod tests {
    use super::Decoder;

    #[test]
    fn garbage_input_is_an_error_not_a_crash() {
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        for len in [0usize, 1, 7, 64, 511, 4096] {
            let input: Vec<u8> = (0..len)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    seed as u8
                })
                .collect();
            let mut decoder = Decoder::new().unwrap();
            let mut out = vec![0; 1024];
            assert!(decoder.decode_chunk(&input, &mut out).is_err(), "len {len}");
        }
    }

    #[cfg(feature = "encoder")]
    #[test]
    fn chunks_round_trip_with_shared_history() {
        use super::{Encoder, Error};
        let chunks: Vec<Vec<u8>> = (0..4)
            .map(|i| {
                format!("chunk {i}: the quick brown fox jumps over the lazy dog. ")
                    .repeat(300 + i)
                    .into_bytes()
            })
            .collect();
        let mut encoder = Encoder::new().unwrap();
        let encoded: Vec<Vec<u8>> = chunks
            .iter()
            .map(|chunk| encoder.encode_chunk(chunk).unwrap())
            .collect();
        let mut decoder = Decoder::new().unwrap();
        for (chunk, compressed) in chunks.iter().zip(&encoded) {
            let mut out = vec![0; chunk.len()];
            decoder.decode_chunk(compressed, &mut out).unwrap();
            assert_eq!(&out, chunk);
        }
        // A wrong declared size is a typed error.
        let mut decoder = Decoder::new().unwrap();
        let mut short = vec![0; chunks[0].len() - 1];
        assert!(matches!(
            decoder.decode_chunk(&encoded[0], &mut short),
            Err(Error::SizeMismatch { .. })
        ));
        // Truncated input fails.
        let mut decoder = Decoder::new().unwrap();
        let mut out = vec![0; chunks[0].len()];
        let cut = &encoded[0][..encoded[0].len() / 2];
        assert!(decoder.decode_chunk(cut, &mut out).is_err());
    }
}
