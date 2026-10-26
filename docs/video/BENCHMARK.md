# 뮤직비디오 분석 성능: `audeniq-music video probe` vs FFprobe

측정일 2026-10-26. Intel Xeon @ 2.80GHz 4코어(클라우드 VM), `release-music` 빌드(엔진 코드는 `bd206ec`),
FFprobe 6.1.1(Ubuntu 24.04). 파일마다 한 번 먼저 실행해 페이지 캐시를 데운 뒤 30회 실행했습니다.
시간·CPU는 중앙값, RSS는 30회 중 최대값(자식 프로세스의 `wait4` rusage)입니다.
FFprobe는 같은 정보를 내는 `-show_streams -show_format -of json`입니다.

재현:

```sh
cargo build --profile release-music --locked -p audeniq-music
python3 -I crates/media-video/bench.py target/release-music/audeniq-music FILE... [--runs 30]
```

## 결과

| 파일 | 크기 | 도구 | 시간(중앙값) | CPU | 최대 RSS | 백엔드 범위 읽기 |
|---|---|---|---|---|---|---|
| h264_4min_faststart.mp4 | 475MB | audeniq-music video probe | 4.5ms | 4.1ms | 12.3MiB | 3회, 0.22MB |
| h264_4min_faststart.mp4 | 475MB | ffprobe | 68.2ms | 67.7ms | 55.7MiB |  |
| | | 배수 | ×15.2 | | | |
| h264_4min_end.mp4 | 475MB | audeniq-music video probe | 4.7ms | 4.3ms | 12.3MiB | 3회, 0.22MB |
| h264_4min_end.mp4 | 475MB | ffprobe | 68.6ms | 65.9ms | 55.7MiB |  |
| | | 배수 | ×14.5 | | | |
| h264_4min_frag.mp4 | 475MB | audeniq-music video probe | 6.7ms | 6.1ms | 11.6MiB | 121회, 0.99MB |
| h264_4min_frag.mp4 | 475MB | ffprobe | 93.0ms | 89.6ms | 61.7MiB |  |
| | | 배수 | ×13.9 | | | |
| long_10min_60fps.mp4 | 100MB | audeniq-music video probe | 6.9ms | 6.5ms | 18.3MiB | 3회, 0.69MB |
| long_10min_60fps.mp4 | 100MB | ffprobe | 62.4ms | 60.9ms | 53.6MiB |  |
| | | 배수 | ×9.0 | | | |
| hevc_4k_hdr.mov | 15MB | audeniq-music video probe | 4.7ms | 4.5ms | 12.0MiB | 3회, 0.03MB |
| hevc_4k_hdr.mov | 15MB | ffprobe | 71.7ms | 69.5ms | 58.6MiB |  |
| | | 배수 | ×15.1 | | | |
| prores_hq.mov | 163MB | audeniq-music video probe | 4.5ms | 4.2ms | 12.0MiB | 2회, 0.02MB |
| prores_hq.mov | 163MB | ffprobe | 78.4ms | 75.6ms | 55.1MiB |  |
| | | 배수 | ×17.3 | | | |

`백엔드 범위 읽기`는 업로드 완료 때 백엔드(`TAE-OK-11/audeniq`의 `mp4::index`)가 S3에 보내는 요청 수와 받는 바이트입니다.
8KiB 창으로 상자 머리와 작은 `ftyp`·`moof`를 한 번에 읽습니다. 창 없이 상자마다 읽던 방식은 일반 파일 6회, 조각 MP4 365회였습니다.

프로세스 시작만 따로 재면(`video capabilities` / `ffprobe -version`, 30회 중앙값):

| 도구 | 시간 | 최대 RSS |
|---|---|---|
| `audeniq-music` | 3.3ms | 10.8MiB |
| `ffprobe` | 52.1ms | 44.7MiB |

시작 비용을 빼면 분석 자체는 엔진 1~4ms, FFprobe 14~53ms입니다.

## 파일

모두 FFmpeg 6.1.1로 만든 2채널 48kHz 소리 포함 파일입니다.

| 파일 | 내용 |
|---|---|
| `h264_4min_faststart.mp4` | 1920×1080 29.97fps H.264(ultrafast, GOP 60) + AAC 256k, 4분, 색인 앞 |
| `h264_4min_end.mp4` | 같은 스트림, 색인 뒤 |
| `h264_4min_frag.mp4` | 같은 스트림, 2초 조각 MP4(`moof` 120개) |
| `long_10min_60fps.mp4` | 320×180 60fps H.264 + AAC, 10분(프레임 36,000개: 색인이 큰 경우) |
| `hevc_4k_hdr.mov` | 3840×2160 24fps HEVC Main10 PQ(BT.2020) + AAC, 10초 |
| `prores_hq.mov` | 1920×1080 23.976fps ProRes 422 HQ + PCM 24비트, 20초 |

## 해석

- 영상 데이터를 읽지 않으므로 파일 크기(15MB~475MB)와 무관하고, 색인 크기(프레임 수·조각 수)에 비례합니다.
- 가장 무거운 10분 60fps도 7ms, 18MiB입니다. 8GB 원본이라도 색인은 수 MB입니다.
- 실제 업로드에서는 S3 왕복 시간(요청 수 × 지연)이 엔진 시간보다 큽니다. 일반 MP4/MOV는 2~3회입니다.

# 소리: 디코드·측정·변환 vs FFmpeg

측정일 2026-10-27. Intel Xeon @ 2.10GHz 4코어(클라우드 VM, AVX2), `release-music` 빌드, FFmpeg 6.1.1(Ubuntu 24.04).
4분 뮤직비디오(1080p H.264 8Mbps + 소리), 3회 실행의 중앙값. CPU는 user+sys, 메모리는 최대 RSS(`wait4`)이며
작은 C 도우미에서 fork해 재서 측정기 자신의 메모리가 섞이지 않습니다. probe는 페이지 캐시를 데운 뒤 5회입니다.

재현(CI에서는 `mv-benchmark.yml`이 x86-64와 ARM 러너에서 같은 것을 돌립니다):

```sh
cargo build --profile release-music --locked -p audeniq-music
cargo build --profile release-music --locked -p media-video --example decode_sound
python3 crates/media-video/bench/compare_ffmpeg.py --seconds 240
```

| 작업 | 파일 | 엔진 | FFmpeg | 빠르기 | CPU | 메모리(엔진 / FFmpeg) |
|---|---|---|---|---|---|---|
| 분석 | AAC-LC | 0.004s | 0.056s | ×13.3 | ×13.8 | 12.7 / 55.7MB |
| 디코드 | AAC-LC 44.1k | 0.54s | 0.92s | ×1.69 | ×1.96 | 4.0 / 55.7MB |
| 디코드 | MP3 320k | 0.46s | 0.92s | ×2.00 | ×2.29 | 3.8 / 54.9MB |
| 디코드 | PCM24 48k | 0.23s | 0.92s | ×3.97 | ×5.22 | 3.6 / 54.6MB |
| 디코드 | HE-AAC | 0.44s | 0.73s | ×1.65 | ×1.83 | 5.8 / 51.0MB |
| 디코드 | HE-AACv2 | 0.41s | 0.72s | ×1.78 | ×1.92 | 5.8 / 51.3MB |
| 음량·트루피크 측정 | AAC-LC | 0.73s | 2.33s | ×3.18 | ×3.14 | 21.1 / 56.6MB |
| 음량·트루피크 측정 | MP3 | 0.62s | 2.26s | ×3.67 | ×3.69 | 19.1 / 55.6MB |
| 음량·트루피크 측정 | PCM24 | 0.44s | 2.36s | ×5.32 | ×5.31 | 20.9 / 55.6MB |
| 음량·트루피크 측정 | HE-AAC | 0.74s | 2.39s | ×3.24 | ×3.14 | 27.3 / 54.5MB |
| 음량·트루피크 측정 | HE-AACv2 | 0.60s | 2.50s | ×4.16 | ×4.11 | 27.4 / 54.4MB |
| 변환 aac-320 | AAC-LC 44.1k | 6.80s | 9.23s | ×1.36 | ×1.26 | 25.1 / 58.3MB |
| 변환 aac-320 | MP3 44.1k | 6.75s | 9.35s | ×1.38 | ×1.29 | 25.0 / 57.6MB |
| 변환 aac-320 | PCM24 48k | 6.25s | 9.84s | ×1.57 | ×1.54 | 25.1 / 57.6MB |
| 변환 pcm24 | AAC-LC 44.1k | 1.10s | 1.26s | ×1.15 | ×1.12 | 25.3 / 56.3MB |
| 변환 pcm24 | MP3 44.1k | 1.18s | 1.60s | ×1.35 | ×1.27 | 23.1 / 55.2MB |
| 변환 pcm24 | PCM24 48k | 0.69s | 1.24s | ×1.79 | ×2.38 | 25.0 / 55.4MB |

FFmpeg 명령: 디코드 `-i F -map 0:a:0 -f f32le /dev/null`, 측정 `-af ebur128=peak=true -f null -`,
변환 `-c:v copy -c:a aac -b:a 320k -ar 48000 -ac 2 -movflags +faststart` / `-c:a pcm_s24le -ar 48000 -ac 2`.
엔진의 44.1→48kHz 리샘플은 카이저 192탭(FFmpeg 기본 swr보다 긴 필터)입니다.

어디서 빨라졌나:

- 공용 커널(`media-dsp`): FFT·DCT-IV를 8레인 블록으로 벡터화하고, SBR의 32슬롯처럼 독립된 작은 변환은 슬롯을
  벡터 레인으로 한 번에 돌립니다. AVX2는 런타임 선택, AArch64는 NEON 기본(같은 결과).
- 공용 비트스트림(`media-bits`): 캐시 워드 비트 리더와 9비트 표 조회 허프만을 모든 코덱이 함께 씁니다.
- SBR QMF는 크기 64 DCT-IV로, MP3 IMDCT는 크기 18 DCT-IV, 합성은 크기 32 DCT-II로 줄였습니다.
- 변환은 새 소리를 메모리에 모으지 않고 임시 파일로 흘려 쓴 뒤 파일→파일(`copy_file_range`)로 옮기며,
  디코드와 리샘플·인코드를 두 스레드에서 겹칩니다.
