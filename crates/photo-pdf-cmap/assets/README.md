See https://github.com/LaurenzV/cmap-resources/tree/hayro for information on how this blob was generated and how
it can be parsed.

`cmaps.zlib` is that project's `cmaps.brotli` blob decompressed and recompressed as zlib (Python
`zlib.compress(data, 9)`), so it is read with `photo-deflate` instead of a brotli decoder.
Decompressed SHA-256: `230f46e3068770bf0e2c515e95473e46df6f0854eff3552e48e355e8ab148868`.
