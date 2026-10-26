# AUDENIQ 미디어 엔진

AUDENIQ 백엔드가 쓰던 외부 미디어 도구(FFmpeg·FFprobe, ExifTool, ZBar, Poppler, Tesseract,
Python/Pillow 정화기)를 대체하는 Rust 엔진입니다. 뮤직비디오 배급으로 늘어난 영상 분석(FFprobe)도 `media-video`가 맡습니다. 오디오 엔진(`audeniq-qc`)과 이미지·PDF 도구(`audeniq-photo`)를
하나의 Cargo 워크스페이스로 합치고, 두 쪽이 따로 갖고 있던 공통 부분을 공유 크레이트로 모았습니다.

- 오디오: 무손실 디코딩/FLAC 정규화, LUFS·True Peak QC, 지문 입력 — [docs/audio/README.md](docs/audio/README.md)
- 이미지·PDF: 메타데이터, 색상·출처 보고, QR, OCR, 업로드 정화, PDF — [docs/photo/README.md](docs/photo/README.md)
- 뮤직비디오: MP4/MOV 분석(FFprobe 필드), 소리 디코드·음량 측정(자체 AAC·HE-AAC·MP3 디코더), 배급 규격 검사, 배급 사양 소리 변환 — [docs/video/README.md](docs/video/README.md)
- Dolby Atmos: ADM BWF 마스터(BW64/RF64) 읽기와 공간 음향 납품 검사 — [docs/atmos/README.md](docs/atmos/README.md)

## 구성

| 경로 | 역할 |
|---|---|
| `crates/media-hash` | **공통** MD5·SHA-256·CRC32 (SHA-NI/ARMv8 SHA2, PCLMUL/VPCLMUL/PMULL 런타임 선택), MD5+SHA-256 융합 경로 |
| `crates/media-json` | **공통** JSON 값·출력·파서 — serde_json 1.0과 바이트 단위로 같은 출력, 외부 크레이트 없음 |
| `crates/audeniq-qc` | 오디오 엔진 라이브러리·명령행(`cli`)과 `audeniq-qc` 바이너리 |
| `crates/audeniq-qc-tools` | 오디오 검증·벤치마크 도구 (`audeniq-qc-tools`) |
| `crates/audeniq-photo` | 이미지·PDF 통합 API |
| `crates/photo-cli` | photo 명령행 라이브러리와 `audeniq-photo` 바이너리 |
| `crates/media-video` | 뮤직비디오 MP4/MOV 분석·소리 측정·배급 검사 (`audeniq-music video …`) |
| `crates/media-aac` | AAC-LC 디코더·인코더, HE-AAC(SBR)·HE-AACv2(PS) 디코드 (뮤직비디오 소리) |
| `crates/media-mp3` | MP3(MPEG-1/2/2.5 Layer III) 디코더 (뮤직비디오 소리) |
| `crates/media-adm` | Dolby Atmos ADM BWF 분석·납품 검사 (`audeniq-music atmos …`) |
| `crates/audeniq-music` | 백엔드가 설치하는 단일 바이너리 `audeniq-music` (`qc …`, `photo …`, `video …`, `atmos …`) |
| `crates/photo-bench` | 외부 도구 대비 벤치마크 (`audeniq-photo-bench`) |
| `crates/photo-*` | 코덱·파서: core, deflate, png, jpeg, icc, meta, qr, ocr, pdf(+syntax/interpret/ccitt/jbig2/jpeg2000/cmap/postscript) |

통합으로 바뀐 공통 부분:

- **해시**: qc의 `src/{md5,sha256,crc32,hash_fused}.rs`와 photo의 `photo-hash` 복사본 → `media-hash` 한 벌.
  photo의 PNG CRC·zlib·PDF 암호 경로와 오디오의 PCM SHA-256·FLAC MD5·TTA CRC가 같은 커널을 씁니다.
- **JSON**: qc의 `src/json.rs` → `media-json`. photo의 serde_json 의존을 없앴습니다(출력은 바이트 단위로 동일).
- **툴체인·잠금·CI**: `rust-toolchain.toml`(1.98.1), `Cargo.lock`, 워크스페이스 린트·프로필을 하나로.

## 빌드

백엔드는 오디오와 photo를 한 실행 파일 `audeniq-music`으로 씁니다.

```sh
cargo build --profile release-music --locked -p audeniq-music  # target/release-music/audeniq-music
```

```text
audeniq-music qc <명령> ...      audeniq-qc와 같은 명령·JSON (예: audeniq-music qc analyze FILE --fingerprint)
audeniq-music photo <명령> ...   audeniq-photo와 같은 명령·출력 (예: audeniq-music photo sanitize SRC DST MIME)
audeniq-music video <명령> ...   뮤직비디오 분석 (예: audeniq-music video probe --sound FILE)
audeniq-music atmos <명령> ...   Dolby Atmos 마스터 분석 (예: audeniq-music atmos probe --peaks FILE)
audeniq-music --version          audeniq-music 0.1.0 (audeniq-qc/0.1.0)
```

`qc` 쪽은 패닉 훅으로 통합 전과 같이 패닉 시 abort(SIGABRT)합니다. 엔진 버전(`audeniq-qc/0.1.0`)과 출력은 바뀌지 않습니다.

측정·검증용으로 두 엔진을 따로 빌드할 수도 있습니다. 성능 특성이 달라 release 프로필을 나눕니다.

```sh
cargo build --release --locked -p audeniq-qc                 # target/release/audeniq-qc
cargo build --profile release-photo --locked -p photo-cli    # target/release-photo/audeniq-photo
```

| 프로필 | 설정 | 이유 |
|---|---|---|
| `release` | opt-level 3, LTO 없음, panic=abort (통합 전 audeniq-qc와 동일) | 오디오 커널은 런타임 디스패치. fat LTO·codegen-units=1이면 ALAC→FLAC+QC CPU 시간이 약 13% 증가. unwind면 시작할 때마다 동적 심볼 해석이 늘어남 |
| `release-photo` | `release` + fat LTO, codegen-units=1, strip, panic=unwind (통합 전 audeniq-photo와 동일) | 크레이트 사이 인라이닝이 중요(LTO 없이 OCR 약 11% 느림) |
| `release-music` | `release-photo` + thin LTO, codegen-units 16 | 한 바이너리라 unwind가 필요. fat LTO면 ALAC 변환·분석이 약 11%, codegen-units=1이면 약 26% 느려짐. thin LTO는 오디오가 `release`와 같고 JPEG 정화만 약 10% 느림 ([docs/BENCHMARK-MUSIC.md](docs/BENCHMARK-MUSIC.md)) |

photo는 디코더 패닉을 `catch_unwind`로 격리해야 하므로 `audeniq-photo`는 unwind가 아닌 프로필(`--release`)로는
컴파일 오류를 내며 빌드되지 않습니다.

통합 전후 성능 비교(방법·결과)는 [docs/BENCHMARK-MERGE.md](docs/BENCHMARK-MERGE.md)에 있습니다.
보안 점검 결과와 퍼징 방법은 [docs/SECURITY-AUDIT.md](docs/SECURITY-AUDIT.md), [fuzz/README.md](fuzz/README.md)에 있습니다.

## 테스트

```sh
cargo fmt --all --check
cargo clippy --workspace --locked --all-targets -- -D warnings
cargo test --workspace --locked
```

photo와 video의 대조 테스트는 exiftool, zbarimg, qrencode, ffmpeg, poppler, python3/Pillow가 있을 때만 비교하고
없으면 건너뜁니다(`.github/workflows/ci.yml`이 설치). 오디오 측정 워크플로(`native.yml`, `media-codecs.yml` 등)는
오디오와 공통 크레이트만 빌드합니다.

## 배포(백엔드) 메모

백엔드(`TAE-OK-11/audeniq`의 `deploy/install-audeniq-music.sh`)는 `deploy/audeniq-music.rev`의 커밋을 고정해
설치하므로 이 저장소의 새 커밋은 그 파일을 올려야 배포에 들어갑니다:

```sh
cargo install --locked --git https://github.com/TAE-OK-11/audeniq-media --rev <rev> --root "$dest" \
  --profile release-music audeniq-music
```

`video`·`atmos`는 이 바이너리에만 있습니다. 명령 이름 앞에 `qc`/`photo`가 붙는 것 말고 명령행·JSON 출력은 `audeniq-qc`, `audeniq-photo`와 같습니다.

## 라이선스

크레이트마다 `Cargo.toml`의 `license`를 따릅니다. 오디오 엔진과 공통 크레이트는 LGPL-2.1-or-later
([LICENSE](LICENSE), [COPYING.GPLv2](COPYING.GPLv2), 출처는 [THIRD_PARTY.md](THIRD_PARTY.md)),
photo 크레이트는 워크스페이스 기본값(`UNLICENSED`) 또는 각 업스트림 라이선스(hayro 계열 Apache-2.0 OR MIT 등)입니다.
