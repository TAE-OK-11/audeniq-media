# 뮤직비디오 분석 (`media-video`, `audeniq-music video`)

뮤직비디오 배급이 생기면서 백엔드에 필요해진 FFprobe 역할(영상 파일이 무엇인지, 배급할 수 있는지)을
대체하는 Rust 엔진입니다. 외부 크레이트 없이 `media-json`만 씁니다.

```text
audeniq-music video probe FILE      보고서 JSON (stdout, 종료 0)
audeniq-music video capabilities    엔진·규칙 버전
```

오류는 `audeniq-music qc`와 같은 약속입니다: stderr에 `{"detail":…,"error":"IO|INVALID|LIMIT|USAGE"}`, 종료 코드 2.

## 무엇을 읽나

상자 머리, `ftyp`, `moov`, `moof`만 읽고 `mdat`(영상·소리 데이터)는 건너뜁니다. 8GB 원본도 몇 번의 짧은 읽기로 끝나고,
백엔드는 S3에서 상자 머리와 색인만 범위 읽기로 가져와 같은 크기의 희소 파일로 넘겨도 됩니다(데이터 자리는 0).
그래서 **보고서는 메타데이터입니다**: 프레임이 깨진 영상도 색인이 맞으면 깨끗하게 나옵니다.

| 영역 | 내용 |
|---|---|
| 컨테이너 | MP4(ISO BMFF)·QuickTime MOV, 64비트 상자, `moov` 앞/뒤(fast start), 조각 MP4(`moof/traf/tfhd/trun`, `trex` 기본값) |
| 트랙 | `tkhd`(활성, 회전 행렬), `mdhd`(timescale, 길이, 언어), `hdlr`, `edts/elst`(시작 지연, 표시 길이), `dref`(외부 미디어 참조) |
| 샘플 표 | `stts`(프레임 수·고정/가변 프레임레이트), `stsz`/`stz2`, `stsc`+`stco`/`co64`(청크 위치 → 파일 끝·`mdat` 밖 검사), `stss`(키프레임 간격) |
| 영상 코덱 | H.264 `avcC`+SPS(프로파일·레벨·크롭 크기·크로마·비트 깊이·필드 코딩·VUI SAR/색), HEVC `hvcC`+SPS(VUI 포함), AV1 `av1C`, VP9 `vpcC`, ProRes 종류(fourcc), `colr`(nclx/nclc), `pasp`, `fiel`, Dolby Vision 상자 |
| 오디오 코덱 | AAC `esds`(AudioSpecificConfig: LC/HE-AAC/HE-AACv2, 실제 샘플레이트·채널), QuickTime 사운드 설명 v0/v1/v2, LPCM(`lpcm`·`sowt`·`twos`·`in24`·`in32`·`fl32`·`fl64`·`ipcm`), ALAC, FLAC(`dfLa`), Opus, AC-3/E-AC-3 |

필드 이름은 FFprobe와 같습니다(`codec_name`, `profile`, `level`, `pix_fmt`, `r_frame_rate`, `avg_frame_rate`,
`nb_frames`, `color_*`, `field_order` 등). `rotation`은 FFprobe 디스플레이 행렬처럼 반시계 방향 각도입니다.
`tests/ffprobe.rs`가 FFmpeg로 만든 H.264(일반·풀레인지·인터레이스·비정사각 픽셀·VFR·회전·조각)·HEVC Main10 HDR·ProRes HQ·
VP9·AV1·MPEG-4 영상과 AAC·PCM·ALAC·FLAC·Opus 소리를 FFprobe 결과와 비교합니다(FFmpeg가 없으면 건너뜀).
FFprobe와 다르게 두는 것: AV1 색 정보(시퀀스 헤더 OBU)는 읽지 않고, 길이는 편집 목록이 있으면 그 길이입니다(FFprobe는
스트림에 따라 미디어 길이를 쓰기도 해 한두 프레임 차이).

## 배급 검사 (`mv-delivery-v1`)

`issues`의 각 항목은 `code`, `severity`, `detail`(한국어 안내)입니다. `error`가 하나도 없으면 `mv_ready: true`.

| 등급 | 코드 |
|---|---|
| error (그대로는 배급 불가) | `TRUNCATED`, `MEDIA_OUTSIDE_MDAT`, `INDEX_INCONSISTENT`, `EXTERNAL_MEDIA`, `NO_VIDEO`, `NO_VIDEO_FRAMES`, `NO_AUDIO`, `NO_AUDIO_FRAMES` |
| warning (플랫폼이 거절하거나 담당자가 봐야 함) | `VIDEO_CODEC`(H.264·HEVC·ProRes 외), `RESOLUTION_BELOW_1080P`(짧은 변), `ROTATED`, `NON_SQUARE_PIXELS`, `INTERLACED`, `VARIABLE_FRAME_RATE`, `FRAME_RATE_UNUSUAL`(23.976·24·25·29.97·30·50·59.94·60 외), `H264_HIGH_BIT_DEPTH`, `VIDEO_BIT_RATE_LOW`(H.264 1080p 8/12Mbps, 1440p 16/24, 2160p 35/53 — 30fps 이하/초과, HEVC는 절반, ProRes 제외), `MULTIPLE_VIDEO`, `MULTIPLE_AUDIO`, `AUDIO_CODEC`(AAC-LC·PCM·ALAC 외), `AUDIO_SAMPLE_RATE`(44.1kHz 미만), `AUDIO_CHANNELS`(스테레오 아님), `AUDIO_BIT_RATE_LOW`(AAC 192kbps 미만), `DURATION_MISMATCH`(1초 초과), `AV_OFFSET`(시작 100ms 초과 어긋남), `DURATION_UNKNOWN` |
| info | `HDR`(PQ·HLG·Dolby Vision), `INDEX_AT_END` |

규칙을 바꾸면 `RULE_VERSION`을 올립니다. 백엔드는 보고서의 `rule`을 같이 저장합니다.

## 한계

- 프레임을 디코딩하지 않습니다(깨진 프레임, 실제 화면 내용, 음량은 보지 않음). 담당자 확인이나 별도 디코드 검사가 필요합니다.
- 색인 한도: `moov` 64MiB, `moof` 하나 16MiB, 트랙당 샘플 5천만 개, 최상위 상자 20만 개. 넘으면 `LIMIT`.
- MPEG-4 Part 2·MJPEG·DNxHD 등은 코덱 이름만 알려 줍니다(프로파일·픽셀 형식 없음).

## 측정

4분 640×360 H.264+AAC MP4(62MB, 7,200프레임)를 `release-music` 빌드로 30번 실행한 중앙값:

| 도구 | 시간 | 최대 RSS |
|---|---|---|
| `audeniq-music video probe` | 6.0ms | 14.4MiB |
| `ffprobe -show_streams -show_format -of json` (6.1.1) | 64.7ms | 52.3MiB |

퍼징: `fuzz/fuzz_targets/video.rs` ([fuzz/README.md](../../fuzz/README.md)), 테스트의 `corrupted_indexes_do_not_panic`은
색인 바이트를 4,000번 무작위로 바꿔 패닉이 없는지 봅니다.
