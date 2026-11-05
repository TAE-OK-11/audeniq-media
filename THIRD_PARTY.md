# Licensing and third-party provenance

This repository is private. AUDENIQ's own code is proprietary
(`LicenseRef-AUDENIQ-Proprietary`, see [LICENSE](LICENSE)). The files listed
below come from, or are ported from, third-party software and keep their
original licenses; their license texts are in [LICENSES/](LICENSES) or next
to the crate. Each crate's `Cargo.toml` `license` field states the
combination (`LicenseRef-AUDENIQ-Proprietary AND <upstream>`), and every
crate has `publish = false`.

The obligations of these licenses mostly attach to *distribution* (handing
source or binaries to someone outside AUDENIQ). Running the engine on
AUDENIQ's own servers is not distribution. Keep this file and `LICENSES/`
with any copy that leaves AUDENIQ (a shipped binary, a source drop to a
partner), and include the notices below in that copy's documentation.

## Summary by crate

| Crate | Origin | License of the third-party part | Text |
|---|---|---|---|
| `audeniq-qc`, `audeniq-qc-tools`, `audeniq-music`, `media-bits`, `media-dsp`, `media-hash`, `media-video`, `media-adm`, `media-resample`, `audeniq-photo`, `photo-cli`, `photo-core`, `photo-png`, `photo-meta`, `photo-bench` | AUDENIQ (see the notes below for formats implemented from specifications) | — | [LICENSE](LICENSE) |
| `media-aac`, `media-mp3` | AUDENIQ code; **only** the table files `media-aac/src/tables.rs`, `sbr_tables.rs`, `ps_tables.rs` and `media-mp3/src/tables.rs` are transcribed from FFmpeg | LGPL-2.1-or-later (those four files) | [LICENSES/LGPL-2.1-or-later.txt](LICENSES/LGPL-2.1-or-later.txt) |
| `media-json` | AUDENIQ; `src/ryu.rs` and its tables port Ryu `d2d` (Ulf Adams) | Apache-2.0 OR BSL-1.0 | [Apache-2.0](LICENSES/Apache-2.0.txt), [BSL-1.0](LICENSES/BSL-1.0.txt) |
| `photo-deflate` | `src/engine` is the zlib-rs 0.6.8 engine (Trifecta Tech Foundation), modified ([ENGINE.md](crates/photo-deflate/ENGINE.md)) | Zlib | [crates/photo-deflate/LICENSE-zlib-rs](crates/photo-deflate/LICENSE-zlib-rs) |
| `photo-jpeg` | Decoder and baseline encoder ported from libjpeg-turbo's libjpeg API code (`jidctint.c`, `jdsample.c`, `jdcolor.c`, `jdphuff.c`, `jcdctmgr.c`, `jfdctint.c`, ...) | IJG | [LICENSES/IJG.txt](LICENSES/IJG.txt) |
| `photo-icc` | Transforms ported from LittleCMS 2 (Marti Maria Saguer) | MIT | [LICENSES/MIT-LittleCMS.txt](LICENSES/MIT-LittleCMS.txt) |
| `photo-qr` | Identification pipeline ported from quirc (Daniel Beer); the Reed–Solomon decoder is AUDENIQ's | ISC | [LICENSES/ISC-quirc.txt](LICENSES/ISC-quirc.txt) |
| `photo-ocr` | Port of Tesseract 5.3 LSTM recognizer and layout (Google, Tesseract contributors); `src/page/lept.rs` and the preprocessing follow Leptonica (Dan Bloomberg) | Apache-2.0; Leptonica: BSD-2-Clause style | [Apache-2.0](LICENSES/Apache-2.0.txt), [Leptonica](LICENSES/BSD-2-Clause-Leptonica.txt) |
| `photo-ocr/models/*.traineddata` | `tessdata_fast` models, © Google Inc. ([README](crates/photo-ocr/models/README.md)) | Apache-2.0 | [LICENSES/Apache-2.0.txt](LICENSES/Apache-2.0.txt) |
| `photo-pdf`, `photo-pdf-syntax`, `-interpret`, `-ccitt`, `-jbig2`, `-jpeg2000`, `-cmap`, `-postscript` | hayro 0.8.0 (The Hayro Authors), which itself adapts code from PDFBox, pdf.js and the png crate ([UPSTREAM.md](crates/photo-pdf/UPSTREAM.md), [NOTICE-hayro.md](crates/photo-pdf/NOTICE-hayro.md)) | Apache-2.0 OR MIT | [crates/photo-pdf/LICENSE-APACHE-hayro](crates/photo-pdf/LICENSE-APACHE-hayro), [LICENSE-MIT-hayro](crates/photo-pdf/LICENSE-MIT-hayro) |
| `photo-pdf-interpret/assets/Foxit*.pfb` | Fonts from PDFium (© 2014 PDFium Authors, Foxit Software) | BSD-3-Clause | [LICENSE_FOXIT](crates/photo-pdf-interpret/assets/LICENSE_FOXIT) |
| `photo-pdf-interpret/assets/*.icc`, `photo-pdf-jpeg2000/assets/*.icc` | Compact-ICC-Profiles (saucecontrol); `LAB.icc` generated with LittleCMS | CC0-1.0 | [CGATS_LICENSE.txt](crates/photo-pdf-interpret/assets/CGATS_LICENSE.txt), [LICENSE.txt](crates/photo-pdf-jpeg2000/assets/LICENSE.txt) |
| `photo-pdf-cmap/assets/cmaps.zlib` | Adobe predefined CMaps (cmap-resources) | BSD-3-Clause | [LICENSE.txt](crates/photo-pdf-cmap/assets/LICENSE.txt) |
| `audeniq-photo/tests/fixtures/pdf/*.pdf` | hayro test suite ([NOTICE.md](crates/audeniq-photo/tests/fixtures/pdf/NOTICE.md)) | Apache-2.0 OR MIT | hayro texts above |

**IJG acknowledgement** (required by the IJG License for any copy that is
distributed): *This software is based in part on the work of the Independent
JPEG Group.*

### Implemented from specifications (no third-party code)

* `photo-meta` reads TIFF/EXIF, XMP, ID3v2, RIFF INFO/bext, Vorbis comments,
  APEv2, QuickTime/ISO BMFF and PNG text from their format specifications.
  It contains no ExifTool code; it only emits ExifTool-compatible tag and
  group names (each format's own field names, plus the few ExifTool aliases
  marked in the tables) so existing callers see the same keys.
* `audeniq-qc` WavPack decoding follows the WavPack format as defined by
  libwavpack; no libwavpack source is included (its BSD-3-Clause notice is
  kept in [LICENSES/BSD-3-Clause-libwavpack.txt](LICENSES/BSD-3-Clause-libwavpack.txt)
  as a courtesy).
* `photo-icc` profile parsing, `photo-png`, `media-video` (MP4/QuickTime),
  `media-adm` (ITU-R BS.2076/BS.2088), `media-hash` (RFC 1321, FIPS 180-4,
  CRC folding after Gopal et al.) and `media-resample` are AUDENIQ's.

External programs (FFmpeg, ExifTool, zbarimg, Tesseract, Pillow/LittleCMS,
image, serde_json, RustCrypto, crc32fast, Symphonia) appear only as test
oracles, benchmark baselines or dev-dependencies. They are not linked into
the default build and none of their code is in the engine.

## FFmpeg: what remains

All FFmpeg-derived *code* has been replaced (see "Clean-room replacements").
What remains is table **data** of the MPEG standards, transcribed from FFmpeg
n6.1.1 (pinned commit `12c589a37d093cc55618f8377b092dc21416806f`). These four
files keep `SPDX-License-Identifier: LGPL-2.1-or-later` and FFmpeg's
attribution:

* `crates/media-aac/src/tables.rs`: AAC Huffman codebooks, scalefactor band offsets and TNS band limits (ISO/IEC 14496-3) from `libavcodec/aactab.c`; Oded Shimon (2005-2006), Maxim Gavrilov (2006-2007).
* `crates/media-aac/src/sbr_tables.rs`, `crates/media-aac/src/ps_tables.rs`: SBR and parametric-stereo Huffman codebooks, start-frequency offsets, QMF prototype window, noise table and hybrid-band maps (ISO/IEC 14496-3) from `libavcodec/aacsbrdata.h`, `sbrdsp_template.c`, `aacpsdata.c`; Robert Swain (2008-2009), Alex Converse (2008-2010). The window's c(384) and c(512) carry the standard's signs.
* `crates/media-mp3/src/tables.rs`: Layer III Huffman codebooks, scale-factor band widths, pretab, slen/LSF tables (ISO/IEC 11172-3, 13818-3) from `libavcodec/mpegaudiodec_common.c`, and the synthesis window from `mpegaudiodsp_data.c`; Fabrice Bellard (2001-2003).

The values are the standards' normative tables, but because these files were
produced from FFmpeg's sources they are kept under LGPL-2.1-or-later. To make
the engine entirely AUDENIQ-licensed, regenerate them from the ISO/IEC texts
(or another permissively licensed transcription) and check they are
identical; then drop LGPL from `media-aac` and `media-mp3`. Until then, a
binary that is distributed outside AUDENIQ must meet LGPL-2.1 (offer the
source of these files and allow relinking); internal use needs nothing.

No FFmpeg binary, libav* library or native media-codec FFI is linked.

## Clean-room replacements


Modules once derived from FFmpeg, rewritten from public
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
* `crates/audeniq-qc/src/alac.rs`: the ALAC decoder, written from Apple's
  published format (magic cookie, SCE/CPE elements with partial-frame,
  shifted-byte and escape flags, adaptive Golomb residuals with zero runs,
  sign-adaptive predictor, stereo unmixing). Decodes FFmpeg-encoded
  mono/stereo 16/24-bit files bit for bit like the previous decoder and to
  the source PCM, at the same speed. The previous decoder's ALAC Golomb
  helper in `crates/media-bits` was removed with it.
* `crates/audeniq-qc/src/wavpack.rs`: the WavPack 4/5 integer lossless
  decoder. Its format arithmetic (metadata sub-blocks, adaptive median
  code with zero runs, decorrelation terms, integer shifts) follows the
  WavPack 5 file format description and the reference library libwavpack
  (BSD-3-Clause, notice below); its structure is AUDENIQ's (entropy state
  in registers, separate passes, branch-free weight steps, the computed
  exp2 table, the staged value decoder). It adds verification of the
  WavPack 5 block checksum. Decodes the reference encoder's modes
  (`-f`, `-h`, `-hh`, `-x`), mono/stereo, 16/24-bit, to the source PCM
  (`tests/regressions.rs`), FFmpeg-encoded files and 800 corruptions like
  the previous decoder, at the same or better speed.
* `crates/audeniq-qc/src/flac_decode.rs`, `crates/audeniq-qc/src/flac.rs`:
  the FLAC decoder and encoder. Both began as FFmpeg ports and were
  redesigned by AUDENIQ over many rounds (Rice runs and quads, register
  rings, accumulated range checks, inductive frame verification, own cost
  models and stereo trials); the format code that survived from the ports
  (frame header and coded number, metadata and STREAMINFO reading and
  writing) is now written from RFC 9639, and the two Levinson–Durbin
  copies are one function. Encoder output is byte-identical at every
  level; decoding the reference `flac` and FFmpeg encoders' files and 800
  corruptions matches the previous decoder; both are as fast or faster.
* `crates/media-aac/src/sbr.rs`, `crates/media-aac/src/ps.rs`: HE-AAC SBR
  and parametric stereo, rewritten from ISO/IEC 14496-3 (4.6.18, 8.6.4):
  header and frequency band tables, time grid, delta-coded scale factors
  and coupling, covariance predictors and chirp factors, envelope
  adjustment with limiter, boost and smoothing, noise and sinusoids; PS
  bitstream, hybrid filter banks with look-ahead, all-pass decorrelation
  with transient attenuation, Ra/Rb mixing with IPD/OPD and interpolation.
  The QMF banks are AUDENIQ's DCT-IV forms. The previous decoders followed
  FFmpeg step by step; the new ones were written without their code and
  checked only against outputs: the MPEG conformance references give the
  same error as before (al_sbr_cm_48_2 0.5000, al_sbr_sr_48_2_fsaac48
  0.5741, al_sbr_ps_06 0.5001, CT_DecoderCheck 0.2844 LSB; al_sbr_ps_04
  0.2772 against 0.2750), at the same speed.


## Rust dependencies

* Symphonia (MPL-2.0): **optional `reference-codecs` comparison build only**, FLAC/ALAC decoding and FLAC/M4A demuxing. Default builds do not link it. Reference implementations are existing Rust code, not claimed as our ports.
* JPEG/PNG cover validation is implemented in this repository (`crates/audeniq-qc/src/jpeg.rs`, `crates/audeniq-qc/src/png.rs`, `crates/audeniq-qc/src/inflate.rs`). It accepts and rejects the same files as image 0.24.9 (MIT, with jpeg-decoder 0.3.2 and png 0.17.16), which remains only as a dev-dependency test oracle.
* JSON reports and the tools' JSON reading use `crates/media-json` (formerly `src/json.rs`), implemented in this repository. Its output is byte-for-byte what serde 1.0.219 / serde_json 1.0.140 (MIT OR Apache-2.0) wrote, including ryu's shortest float digits. f64 digits come from `crates/media-json/src/ryu.rs`, a port of Ryu's `d2d` (Ulf Adams, https://github.com/ulfjack/ryu, Apache-2.0 OR BSL-1.0) with its power-of-5 tables regenerated by `crates/media-json/scripts/gen_ryu_tables.py`; f32 digits come from Rust's own shortest `{:e}` formatting with ties resolved to even. serde_json remains only as a dev-dependency test oracle.
* SHA-256 (canonical PCM hash; x86 SHA extensions, ARMv8 SHA2 or portable) and the required FLAC PCM MD5 (a format integrity field, not a security authenticator) are implemented in this repository (`crates/media-hash/src/sha256.rs`, FIPS 180-4; `crates/media-hash/src/md5.rs`, RFC 1321). RustCrypto sha2 and md-5 (MIT OR Apache-2.0) remain only as dev-dependency test oracles; the development tools crate uses the in-repository SHA-256, with FFmpeg as its independent hash oracle.
* IEEE CRC-32 (TTA header/frame checks and the frame-copy round-trip check) is implemented in this repository (`crates/media-hash/src/crc32.rs`; carry-less-multiply folding after Gopal et al., Intel 2009, with AVX-512/AVX2 VPCLMULQDQ, PCLMULQDQ, AArch64 PMULL and slicing-by-8 paths). crc32fast (MIT OR Apache-2.0) remains only as a dev-dependency test oracle.

Cargo.lock pins transitive dependencies. Dependency sources/license notices remain available through crates.io. Linking into other applications requires complying with each applicable license.

`crates/audeniq-qc/src/m4a.rs` and `crates/media-bits/src/msb.rs` are AUDENIQ-specific Rust implementations of the container tables and cached bounded bit reader, rather than copied Symphonia source. The audio engine's default build has no third-party crates; the `reference-codecs` build adds Symphonia, and the Rust standard library still applies.

Third-party crates that the default build still links (hayro's
dependencies such as kurbo, skrifa, vello_cpu, moxcms, pic-scale, and
mimalloc) are all permissively licensed (MIT, Apache-2.0, BSD, Zlib,
Unicode-3.0). A distributed binary must carry their notices too; generate
them with `cargo about` or `cargo deny` at that time.

## History

* FFmpeg-derived code (TTA, WavPack, FLAC encoder/decoder, ALAC, the
  K-weighting coefficients from `libavfilter/ebur128.c`, SBR/PS decoding)
  was LGPL-2.1-or-later until the clean-room rewrites above. The libebur128
  MIT notice that came with the K-weighting equations is no longer needed.
* `media-hash` and `media-json` were labelled LGPL because they lived in the
  audio engine; they were always AUDENIQ's own code.
* `COPYING.GPLv2` was removed: LGPL-2.1 does not require the GPL text, and
  no GPL code is present.
