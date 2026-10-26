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
