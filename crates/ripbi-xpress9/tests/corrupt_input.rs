//! Regression inputs found by fuzzing the decoder.

use std::time::{Duration, Instant};

use ripbi_xpress9::Decoder;

/// A 370-byte mutated chunk that made the upstream fetch loop spin forever
/// without consuming input or producing output. The forward-progress guard
/// in `vendor/src/Xpress9DecLz77.c` must reject it promptly.
#[test]
fn stalled_decode_is_rejected_instead_of_hanging() {
    let input = include_bytes!("fixtures/stall-30509.bin");
    let mut out = vec![0; 30509];
    let start = Instant::now();
    let result = Decoder::new().unwrap().decode_chunk(input, &mut out);
    assert!(result.is_err());
    assert!(start.elapsed() < Duration::from_secs(5));
}
