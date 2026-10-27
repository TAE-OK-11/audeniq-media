# Third-party provenance

This file covers the audio engine and the shared crates (`crates/audeniq-qc`,
`crates/audeniq-qc-tools`, `crates/media-hash`, `crates/media-json`). The
photo crates record their sources next to the code: `docs/photo/PORTING.md`,
`crates/photo-deflate/ENGINE.md`, `crates/photo-pdf/UPSTREAM.md`,
`crates/photo-pdf-cmap/assets/README.md` and the OCR model notes.

## FFmpeg-derived modules (LGPL-2.1-or-later)

Pinned FFmpeg commit: `12c589a37d093cc55618f8377b092dc21416806f`.

* `crates/audeniq-qc/src/wavpack.rs`, `crates/audeniq-qc/src/wavpack_table.rs`: `libavcodec/wavpack.c`, `wavpack.h`, `wavpackdata.c`; Konstantin Shishkov (2006, 2011), David Bryant (2020). Only integer lossless paths. Restructured in round 11 (register-resident entropy state, pass structure, branch-free weight steps, own bit reader); the format arithmetic and the attribution remain.
* `crates/audeniq-qc/src/flac.rs`: FLAC fixed/LPC residual prediction, Rice mapping, frame/subframe structure from `libavcodec/flacenc.c` and Welch/Levinson approach from `lpc.c`; Justin Ruggles (2006). Reworked selection, buffers, verification and commit logic; round 12 replaced the Rice estimate with an own cost model and added exact stereo-assignment trials (levels 7-8).
* `crates/audeniq-qc/src/alac.rs`: Rice/zero-run, adaptive LPC and stereo reconstruction from `libavcodec/alac.c`, `alacdsp.c`; David Hammerton (2005). Reworked bounded bit access, in-place reusable buffers and specialized predictor orders; round 12 restructured the adaptive Golomb state and coefficient steps as own functions.
* `crates/audeniq-qc/src/flac_decode.rs`: frame/header, partitioned Rice, fixed/LPC and channel reconstruction from `libavcodec/flac.c`, `flacdec.c`, `flacdsp.c`; Alex Beregszaszi (2003), Mans Rullgard (2012). Reworked streaming input, strict CRC/count/MD5 checks and specialized LPC orders.
* `crates/media-aac/src/tables.rs`: AAC Huffman codebooks, scalefactor band offsets and TNS band limits (ISO/IEC 14496-3 data) generated from `libavcodec/aactab.c` (FFmpeg n6.1.1); Oded Shimon (2005-2006), Maxim Gavrilov (2006-2007). The decoder code in `crates/media-aac` is AUDENIQ's; its window overlap, intensity/noise gain and TNS coefficient conventions were checked against `libavcodec/aacdec_template.c` and match FFmpeg's output sample for sample (`crates/media-aac/tests/ffmpeg.rs`).
* `crates/media-aac/src/sbr_tables.rs`, `crates/media-aac/src/ps_tables.rs`: SBR and parametric-stereo Huffman codebooks, start-frequency offsets, QMF prototype window and noise table (ISO/IEC 14496-3 data) generated from `libavcodec/aacsbrdata.h`, `sbrdsp_template.c` and `aacpsdata.c` (FFmpeg n6.1.1); Robert Swain (2008-2009), Alex Converse (2008-2010). The window's c(384) and c(512) carry the standard's signs (FFmpeg negates them for its MDCT-based banks). `crates/media-aac/src/sbr.rs` and `ps.rs` are AUDENIQ's: the QMF banks are the standard's complex matrices computed as DCT-IVs of size 64 (`crates/media-dsp`, derived in `sbr.rs`); buffer timing, envelope adjustment and PS mixing follow `libavcodec/aacsbr*.c`, `sbrdsp*.c`, `aacps*.c` step for step so the output matches FFmpeg's and the MPEG conformance references (`crates/media-video/tests/conformance.rs`).
* `crates/media-mp3/src/tables.rs`: Layer III Huffman codebooks, scale-factor band widths, pretab, slen/LSF tables (ISO/IEC 11172-3, 13818-3 data) generated from `libavcodec/mpegaudiodec_common.c`, and the synthesis window from `mpegaudiodsp_data.c` (FFmpeg n6.1.1); Fabrice Bellard (2001-2003). The decoder code in `crates/media-mp3` is AUDENIQ's (the IMDCT folded to a DCT-IV of size 18 and the synthesis matrixing to a DCT-II of size 32, both from the standard's definitions); bit-reservoir, intensity-stereo and short-block conventions were checked against `mpegaudiodec_template.c` and match FFmpeg's output within 1e-5 (`crates/media-mp3/tests/ffmpeg.rs`).

Rust translations change storage, bounds handling, error propagation, work limits and dispatch; they do not remove upstream attribution. No FFmpeg binary, libav* library, GPL-only filter, or native media-codec FFI is linked. Media processing is Rust, including CPU intrinsics; normal Rust system-runtime libraries still apply. Full LGPL text: LICENSE; referenced GPL text: COPYING.GPLv2.

## Rust dependencies

* Symphonia (MPL-2.0): **optional `reference-codecs` comparison build only**, FLAC/ALAC decoding and FLAC/M4A demuxing. Default builds do not link it. Reference implementations are existing Rust code, not claimed as our ports.
* JPEG/PNG cover validation is implemented in this repository (`crates/audeniq-qc/src/jpeg.rs`, `crates/audeniq-qc/src/png.rs`, `crates/audeniq-qc/src/inflate.rs`). It accepts and rejects the same files as image 0.24.9 (MIT, with jpeg-decoder 0.3.2 and png 0.17.16), which remains only as a dev-dependency test oracle.
* JSON reports and the tools' JSON reading use `crates/media-json` (formerly `src/json.rs`), implemented in this repository. Its output is byte-for-byte what serde 1.0.219 / serde_json 1.0.140 (MIT OR Apache-2.0) wrote, including ryu's shortest float digits. f64 digits come from `crates/media-json/src/ryu.rs`, a port of Ryu's `d2d` (Ulf Adams, https://github.com/ulfjack/ryu, Apache-2.0 OR BSL-1.0) with its power-of-5 tables regenerated by `crates/media-json/scripts/gen_ryu_tables.py`; f32 digits come from Rust's own shortest `{:e}` formatting with ties resolved to even. serde_json remains only as a dev-dependency test oracle.
* SHA-256 (canonical PCM hash; x86 SHA extensions, ARMv8 SHA2 or portable) and the required FLAC PCM MD5 (a format integrity field, not a security authenticator) are implemented in this repository (`crates/media-hash/src/sha256.rs`, FIPS 180-4; `crates/media-hash/src/md5.rs`, RFC 1321). RustCrypto sha2 and md-5 (MIT OR Apache-2.0) remain only as dev-dependency test oracles; the development tools crate uses the in-repository SHA-256, with FFmpeg as its independent hash oracle.
* IEEE CRC-32 (TTA header/frame checks and the frame-copy round-trip check) is implemented in this repository (`crates/media-hash/src/crc32.rs`; carry-less-multiply folding after Gopal et al., Intel 2009, with AVX-512/AVX2 VPCLMULQDQ, PCLMULQDQ, AArch64 PMULL and slicing-by-8 paths). crc32fast (MIT OR Apache-2.0) remains only as a dev-dependency test oracle.

Cargo.lock pins transitive dependencies. Dependency sources/license notices remain available through crates.io. Linking into other applications requires complying with each applicable license; no claim is made that optimization removes copyleft obligations.

`crates/audeniq-qc/src/m4a.rs` and `crates/audeniq-qc/src/msb.rs` are AUDENIQ-specific Rust implementations of the container tables and cached bounded bit reader, rather than copied Symphonia source. The default build has no third-party crates; the `reference-codecs` build adds Symphonia, and the Rust standard library still applies.

## Clean-room replacements

Modules once derived from the sources above, rewritten from public
specifications without the derived code (each verified against independent
oracles: FFmpeg's binaries, conformance references, the previous output):

* `crates/audeniq-qc/src/meter.rs` K-weighting: the coefficients come from
  ITU-R BS.1770-5's 48 kHz stages, moved to other rates through their exact
  analog prototype (inverse and forward bilinear transform). Integrated
  loudness is unchanged at 48 kHz and within 0.005 LU at 44.1 kHz.
* `crates/audeniq-qc/src/tta.rs`: the TTA1 decoder, written from the
  format's description (header and frame table with CRC-32, two-parameter
  adaptive Rice code, eight-tap sign–sign filter, first-order predictor,
  inter-channel differences). Decodes FFmpeg-encoded mono/stereo 16/24-bit
  files bit for bit like the previous decoder and to the source PCM
  (`tests/regressions.rs`), at the same speed.

## libebur128 MIT notice (historical: no longer used)

Copyright (c) 2011 Jan Kokemüller

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
