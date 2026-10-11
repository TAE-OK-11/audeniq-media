# Source and licensing

Copied from TAE-OK-11/audeniq-qc at commit
`0299d4a123281f1985cc5669b27a62f34320b53b` (LGPL-2.1-or-later):
`src/md5.rs`, `src/sha256.rs`, and `src/crc32.rs`.
The original license is reproduced in LICENSE. This crate retains that license;
the pre-existing Photo parser sources retain their own notices.

Integration adaptations: expose the three streaming types; remove unused QC
dual-hash interleaving hooks; use core slice/hint/architecture imports; gate runtime
CPU detection on std; add Copy/Debug to the scalar CRC state to preserve Photo's
API; provide a local test hex helper. Compression/folding algorithms, constants,
dispatch feature requirements and upstream correctness tests are retained.

No production third-party crate dependency is introduced. Independent reference
implementations are used only by tests; SHA-256 tests reuse Photo's existing
sha2 0.10 dependency. MD5 remains for PDF format compatibility.
Without std, CRC32 and SHA-256 use their portable implementations.
