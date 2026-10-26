# 퍼징

libFuzzer + AddressSanitizer 대상입니다. 워크스페이스 밖의 별도 크레이트라 일반 빌드·테스트에는 들어가지 않습니다.

| 대상 | 입력 | 확인하는 것 |
|---|---|---|
| `deflate` | 임의 바이트 | zlib/raw inflate(신뢰할 수 없는 입력), deflate→inflate 왕복 동일성 (`unsafe` 엔진) |
| `hash` | 임의 바이트 | SHA-256·MD5·CRC32·융합 경로가 RustCrypto/crc32fast와 같은지 (SIMD `unsafe`) |
| `audio` | 파일 | `audeniq-qc` probe·tags·PCM 해시(전체 디코드), AVX2와 스칼라 |
| `image` | JPEG/PNG | probe, cover, QR, 정화(JPEG·PNG·서명) |
| `pdf` | PDF | `pdf::info`, PDF 정화(렌더링 포함) |
| `video` | MP4/MOV | `media-video` 색인 분석과 소리 측정(`audeniq-music video probe --sound`): 상자·샘플 표·조각·코덱 헤더(SPS)·AAC/PCM 디코드 |
| `aac` | ASC 2바이트 + 접근 단위 | `media-aac` AAC-LC 디코더 |
| `mp3` | Layer III 프레임(헤더를 붙이거나 그대로) | `media-mp3` MP3 디코더 |
| `adm` | WAV/RF64/BW64 | `media-adm` 청크·ADM XML·chna·피크 스캔 |

```sh
rustup toolchain install nightly
cargo install cargo-fuzz
cd fuzz
cargo +nightly fuzz build --fuzz-dir . -O -s address
target/x86_64-unknown-linux-gnu/release/pdf corpus/pdf -max_total_time=1800 -timeout=30 -rss_limit_mb=3072
```

`photo-*` API는 패닉을 오류로 바꾸지만, libFuzzer의 패닉 훅이 먼저 실행되므로 잡힌 패닉도 충돌로 보고됩니다.
릴리스 빌드에서 나오지 않는 오버플로도 잡도록 `debug-assertions`·`overflow-checks`를 켭니다.
