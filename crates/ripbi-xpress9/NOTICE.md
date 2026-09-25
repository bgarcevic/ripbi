# Third-party notice

`vendor/` holds Microsoft's XPress9 compression library (Copyright (c)
Microsoft Corporation, MIT License — see the header of each source file).
Microsoft publishes it in
<https://github.com/microsoft/Extensible-Storage-Engine> under
`dev/ese/src/_xpress9/` (MIT; checked against commit
`7030fe7407615160e54d152e4ef704eede2fdd7e`).

The files here were taken from the GCC/Clang port in
<https://github.com/Hugoberry/xpress9-python> at commit
`ff81bd9f93601650243a2695d1ad5940c7fbadd0` (`include/` and `src/`, without
that project's `Xpress9Wrapper` shim); `vendor/LICENSE` is that repository's
MIT license. Relative to Microsoft's copy, the port only changes portability
details: printf format specifiers, alignment attributes, the calling
convention, fixed 64-bit `uxint`/`xint` (identical on every 64-bit target
ripbi ships), disabled SSE paths (encoder only), and a comma fix in a debug
trace.

ripbi's own changes:

- `csrc/ripbi_xpress9.c` is ripbi's shim over the library's public API. It
  replaces the upstream wrapper so that nothing ever writes to stdout or
  stderr.
- `vendor/src/Xpress9DecLz77.c`: `Xpress9DecoderFetchDecompressedData` has a
  forward-progress guard (marked `ripbi:`). Fuzzing found corrupt input on
  which the upstream loop spins forever; two passes in a row that neither
  consume input nor produce output now fail with
  `Xpress9Status_DecoderCorruptedData`. The regression input is
  `tests/fixtures/stall-30509.bin`.
- `build.rs` compiles with `-fno-strict-aliasing`, `-fwrapv`, and
  `-fno-aggressive-loop-optimizations` where supported, matching the MSVC
  semantics the code was written for. Without the last flag, GCC `-O3`
  miscompiles the encoder, which indexes a declared `m_uNext[8]` array past
  its bound by design.
