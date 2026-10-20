# audeniq-music 단일 바이너리: 프로필 선택

백엔드가 `audeniq-qc`와 `audeniq-photo` 두 실행 파일 대신 `audeniq-music` 하나를 설치하도록 바꾸면서
어떤 release 프로필로 빌드할지 비교했습니다. photo가 `catch_unwind`를 쓰므로 한 바이너리는 unwind여야 하고,
LTO·codegen-units만 고를 수 있습니다.

## 측정 방법

- **머신**: Intel Xeon 4 vCPU 공유 VM(AVX2·SHA-NI). 노이즈가 ±5~10%로 큽니다.
- **기준(split)**: `--release`의 `audeniq-qc`, `--profile release-photo`의 `audeniq-photo` (이 커밋, 따로 빌드)
- **시간**: 21라운드, 라운드마다 바이너리 순서를 섞어 코어 3에 고정해 실행. CPU 시간(user+sys) 비율(후보/기준)의
  중앙값과 사분위 범위.
- **명령어 수**: valgrind cachegrind Ir, 15초 클립.
- **입력**: 180초 24/48 스테레오(사인+핑크 노이즈) WAV와 같은 내용의 ALAC, 3000px JPEG·1500px PNG 테스트 패턴.

## 시간 (21라운드, 후보/기준)

| 케이스 | fat LTO, cgu 1 | thin LTO, cgu 1 | **thin LTO, cgu 16 (채택)** |
|---|---|---|---|
| ALAC 24 convert +analyze +fp | 1.106 [1.009, 1.184] | 1.262 [1.156, 1.314] | **1.010** [0.918, 1.128] |
| WAV 24 convert +analyze +fp | 1.002 [0.933, 1.130] | 1.071 [0.971, 1.127] | **1.054** [1.012, 1.198] |
| ALAC 24 analyze +fp | 1.114 [1.073, 1.187] | 1.296 [1.119, 1.331] | **0.918** [0.875, 1.028] |
| JPEG 3000px sanitize | 0.982 [0.944, 1.005] | 1.096 [1.056, 1.148] | **1.101** [1.055, 1.118] |
| JPEG 3000px OCR | 1.015 [0.952, 1.055] | 0.997 [0.935, 1.071] | **0.986** [0.951, 1.037] |

## 명령어 수 (기준 대비)

| 케이스 | 기준 | fat LTO, cgu 1 | LTO 없음 | thin LTO, cgu 16 |
|---|---|---|---|---|
| ALAC convert +analyze +fp | 1074.9M | +0.8% | −0.3% | −0.3% |
| WAV convert l5 | 578.6M | −0.1% | −0.7% | −0.7% |
| FLAC analyze +fp | 728.9M | +1.8% | −0.5% | −0.5% |
| ALAC pcm-hash | 641.4M | −0.5% | −0.5% | −0.5% |
| `qc capabilities` (시작 비용) | 0.5M | +1.0M | +1.0M | +1.0M |
| PNG OCR | 1554.8M | 0.0% | +5.1% | +1.4% |
| JPEG sanitize | 640.9M | 0.0% | +10.7% | +11.5% |
| JPEG cover | 466.7M | 0.0% | −0.7% | +0.9% |

fat LTO의 오디오 회귀는 명령어 수로는 0~2%뿐이고 시간에서 11% 안팎으로 나타납니다(코드 배치 차이,
통합 전 측정의 13%와 같은 원인). JPEG 정화의 thin LTO 회귀는 `photo_jpeg::encoder::encode_rows`
안에서 생깁니다(435M → 506M).

## 결론

곡마다 도는 오디오 QC가 백엔드에서 가장 무거운 작업이라 오디오를 `release`와 같게 유지하는 thin LTO,
codegen-units 16을 `release-music`으로 정했습니다. 대가는 커버·문서 업로드 정화의 JPEG 인코딩 약 10%
(3000px 기준 약 7 ms)입니다. 프로세스 시작 비용은 unwind 런타임과 mimalloc 초기화로 약 1백만 명령어(1 ms 안팎)
늘었습니다.

## 출력

`audeniq-music qc …`/`photo …`의 stdout과 출력 파일은 위 입력 전체에서 기준 바이너리와 바이트 단위로 같습니다
(qc: probe, analyze --fingerprint, fingerprint, pcm-hash, tags, convert --analyze --fingerprint;
photo: probe, meta, color, provenance, qr, ocr, cover, sanitize).
