# 뮤직비디오 분석 (`media-video`, `audeniq-music video`)

뮤직비디오 배급이 생기면서 백엔드에 필요해진 FFprobe 역할(영상 파일이 무엇인지, 배급할 수 있는지)을
대체하는 Rust 엔진입니다. 외부 크레이트 없이 `media-json`만 씁니다.

```text
audeniq-music video probe FILE           보고서 JSON (stdout, 종료 0) — 색인만 읽음
audeniq-music video probe --sound FILE   + 소리 트랙을 디코드해 음량·피크·클리핑·무음 측정
audeniq-music video probe --profile NAME FILE   + 플랫폼 사양 검사(youtube, spotify, apple; --sound와 함께 쓰면 음량 기준까지)
audeniq-music video profiles             프로필별 사양 JSON
audeniq-music video sound-ranges FILE    소리 데이터가 있는 바이트 범위 {"ranges": [[offset, len], …], "bytes": N}
audeniq-music video convert-audio [--preset NAME] IN OUT   영상은 그대로, 소리를 배급 사양으로 변환 (CONVERT.md)
audeniq-music video capabilities         엔진·규칙 버전
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
| 오디오 코덱 | AAC `esds`(AudioSpecificConfig: LC/HE-AAC/HE-AACv2, 하위 호환 SBR 신호, 실제 샘플레이트·채널), QuickTime `chan`(채널 배치), QuickTime 사운드 설명 v0/v1/v2, LPCM(`lpcm`·`sowt`·`twos`·`in24`·`in32`·`fl32`·`fl64`·`ipcm`), ALAC, FLAC(`dfLa`), Opus, AC-3/E-AC-3 |

필드 이름은 FFprobe와 같습니다(`codec_name`, `profile`, `level`, `pix_fmt`, `r_frame_rate`, `avg_frame_rate`,
`nb_frames`, `color_*`, `field_order` 등). `rotation`은 FFprobe 디스플레이 행렬처럼 반시계 방향 각도입니다.
`tests/ffprobe.rs`가 FFmpeg로 만든 H.264(일반·풀레인지·인터레이스·비정사각 픽셀·VFR·회전·조각)·HEVC Main10 HDR·ProRes HQ·
VP9·AV1·MPEG-4 영상과 AAC·PCM·ALAC·FLAC·Opus 소리를 FFprobe 결과와 비교합니다(FFmpeg가 없으면 건너뜀).
FFprobe와 다르게 두는 것: AV1 색 정보(시퀀스 헤더 OBU)는 읽지 않고, 길이는 편집 목록이 있으면 그 길이입니다(FFprobe는
스트림에 따라 미디어 길이를 쓰기도 해 한두 프레임 차이).

## 소리 측정 (`--sound`)

배급 소리(아래 **채널 배치**)를 디코드해 음원 QC와 같은 측정기(`audeniq-qc`의 EBU R128 통합 음량, True Peak,
클리핑, 50ms 블록 무음)로 잽니다. 결과는 보고서의 `sound`(못 재면 `sound_note`에 이유)입니다.

- AAC: 자체 디코더 `crates/media-aac` (아래). AAC-LC, HE-AAC(SBR), HE-AACv2(PS) 모두 출력 샘플레이트 그대로 디코드합니다.
  SBR이 스트림 안에만 있는 경우(암시적 신호)는 색인으로 알 수 없어서, `--sound`로 디코드하면 보고서의 `audio`
  프로파일·샘플레이트·채널을 디코더가 찾은 값(`sound.profile`)으로 고칩니다(FFprobe가 첫 프레임을 디코드해 알리는 것과 같음).
- MP3: 자체 디코더 `crates/media-mp3` (아래).
- ALAC: 음원 엔진(`audeniq-qc`)의 ALAC 디코더를 그대로 씁니다(모노·스테레오).
- PCM: QuickTime/ISO 정수·실수 PCM(`sowt`·`twos`·`in24`·`in32`·`fl32`·`fl64`·`lpcm`·`ipcm`).
- 스피커 배치가 있는 3채널 이상(5.1 등)은 측정하지 않습니다(`sound_note`). Opus·FLAC·AC-3/E-AC-3은 아직 디코드하지 않습니다.
- 조각 MP4도 측정합니다(`trun`마다 표본 위치를 기록, 트랙당 200만 개까지).
- 소리 데이터는 트랙당 256MiB까지 읽습니다(24비트 48kHz 스테레오 PCM 약 15분).
- `sound-ranges`가 알려 주는 범위만 있으면 측정 결과가 같습니다(테스트로 확인). 백엔드는 이 범위만 S3에서 받아옵니다.
  모노 트랙 두 개를 L/R로 쓰는 파일이면 두 트랙의 범위가 모두 나옵니다.

### 채널 배치 (`sound.mapping`, 변환의 `mapping`)

측정과 변환은 같은 규칙으로 소리를 고릅니다(`src/source.rs`).

| `mapping` | 경우 |
|---|---|
| `mono`, `stereo` | 첫 번째로 켜진 소리 트랙(하나뿐이면 그 트랙) 그대로 |
| `dual-mono` | 그 트랙이 모노이고 바로 다음 소리 트랙도 같은 샘플레이트의 모노: 왼쪽·오른쪽으로 합침(편집기가 스테레오를 모노 트랙 두 개로 내보내는 경우) |
| `downmix` | 스피커 배치를 아는 3채널 이상(AAC 채널 구성, QuickTime `chan`, 배치 없는 6채널 PCM은 5.1로 봄): 변환할 때 스테레오로 접음 |
| `channels` | 스피커 배치가 없는(디스크리트, 스템) 3채널 이상: 1·2채널. 또는 `convert-audio --channels`로 고른 채널 |

FFmpeg `ebur128`과의 비교(`tests/ffprobe.rs`): AAC MP4·PCM 24비트 MOV·PCM 16비트 빅엔디언 MOV·조각 MP4에서
통합 음량 ±0.15 LU, True Peak ±0.2 dB 이내. 4분 AAC 스테레오 전체 측정 0.73초(FFmpeg `ebur128` 2.33초, `release-music`).

### AAC 디코더 (`media-aac`)

ISO/IEC 14496-3 AAC-LC: SCE·CPE·LFE, 모든 창 순서·모양(사인/KBD), M/S·인텐시티 스테레오, PNS, TNS, 펄스,
IMDCT(DCT-IV를 1/4 크기 복소 FFT로). 채널 구성 0(PCE)도 읽습니다. Huffman·밴드 경계 표는 FFmpeg `aactab.c`에서 옮겼으므로(생성 스크립트 결과)
크레이트 라이선스는 LGPL-2.1-or-later입니다. 코드는 자체 작성입니다.

- FFmpeg 디코더와 표본 단위 비교(`crates/media-aac/tests/ffmpeg.rs`): 48/44.1/32/22.05kHz, 모노·스테레오·5.1,
  M/S·IS·TNS 각각과 모두 켠 경우, 64k~384k에서 최대 차이 2.4e-7(float 반올림 수준). PNS는 난수라 블록 에너지 1dB 이내.
- 손상된 접근 단위 3,000개에서 패닉 없음, 퍼징 대상 `fuzz/fuzz_targets/aac.rs`.
- 4분 256k 스테레오 디코드 0.54초(FFmpeg 0.92초). 허프만은 9비트 표 조회(`media-bits`), IMDCT는 `media-dsp`의 DCT-IV.

**HE-AAC (SBR, `src/sbr.rs`)·HE-AACv2 (PS, `src/ps.rs`)**: QMF 분석(32밴드)·HF 생성(패치, 역필터)·엔벨로프 조정(노이즈·사인파·리미터)·
QMF 합성(64밴드, 같은 샘플레이트로 신호된 SBR은 32밴드 다운샘플 합성), PS의 하이브리드 분석·역상관·IID/ICC/IPD/OPD 믹싱.
신호 방식은 명시(객체 5·29), 하위 호환(sync extension 0x2b7/0x548), 암시(첫 접근 단위의 SBR) 모두 FFmpeg와 같이 처리하고,
모노 HE-AAC는 PS 데이터가 없어도 스테레오로 냅니다(FFmpeg와 같음). QMF는 표준의 복소 행렬을 크기 64 DCT-IV로 바꿔 계산하고
(유도는 `src/sbr.rs`), 프레임의 32슬롯을 벡터 레인으로 한 번에 돌립니다. 버퍼 시점·이득 계산은 FFmpeg와 같은 순서라
결과가 FFmpeg와 표본 단위로 같습니다. SBR 표는 FFmpeg에서 옮겼고, QMF 창의 c(384)·c(512)는 표준 부호(FFmpeg는 자기 MDCT 구현에 맞춰 뒤집어 둠)입니다.

MPEG 적합성 스트림(FFmpeg FATE가 쓰는 것)의 기준 디코더 출력과 비교(`crates/media-video/tests/conformance.rs`,
`FATE_SAMPLES`가 있으면 실행):

| 스트림 | 내용 | 기준 출력과 RMS 차이 (16비트 LSB) | FFmpeg |
|---|---|---|---|
| `al_sbr_cm_48_2` | SBR 스테레오 48kHz | 0.50 | 0.50 |
| `al_sbr_sr_48_2_fsaac48` | 같은 샘플레이트 SBR(다운샘플 합성) | 0.57 | 0.57 |
| `al_sbr_ps_04_new`, `_06_new` | PS, 32kHz, PCE | 0.28, 0.50 | 0.28, 0.50 |
| `CT_DecoderCheck/sbr_{bc,i}-ps_{bc,i,bic}` | 신호 방식 4가지 | 0.28 | 0.28 |

0.5 LSB 안팎은 기준이 16비트로 반올림된 만큼의 차이입니다. 무작위 SBR/PS 페이로드 12,000개(디버그 빌드)에서 패닉 없음.
4.3분 HE-AAC 스테레오 디코드 0.44초(FFmpeg 0.73초), HE-AACv2 0.41초(FFmpeg 0.72초).

### MP3 디코더 (`media-mp3`)

MPEG-1/2/2.5 Layer III: 모노·스테레오, M/S·인텐시티 스테레오, 긴·짧은·혼합 블록, 비트 저장소(앞 프레임 데이터가 일부 없으면
받을 수 있는 그래뉼만), CRC 프레임. 36점 IMDCT는 크기 18 DCT-IV로, 다상 합성의 행렬은 코사인 대칭으로 크기 32 DCT-II로
줄여 32개 서브밴드·18개 슬롯을 벡터 레인으로 한 번에 계산합니다. 허프만·밴드·합성 창 표는 FFmpeg에서 옮겼고
(LGPL-2.1-or-later) 코드는 자체 작성입니다. Free format, Layer I/II는 디코드하지 않습니다.

- FFmpeg 디코더와 비교(`crates/media-mp3/tests/ffmpeg.rs`): LAME으로 만든 44.1/48/22.05/11.025kHz, CBR·VBR, 모노·스테레오에서
  최대 차이 3e-6. ISO 적합성 스트림(`si`·`si_block`·`si_huff`·`sin1k0db`·`compl`·`he_32/44/48khz`)도 1e-5 이내.
- MP3-in-MP4 변환은 편집 목록(인코더 지연)까지 FFmpeg와 표본 단위로 같습니다(`tests/convert.rs`).
- 4분 320k 스테레오 디코드 0.46초(FFmpeg 0.92초). 퍼징 대상 `fuzz/fuzz_targets/mp3.rs`.

## 배급 검사 (`mv-delivery-v2`)

`issues`의 각 항목은 `code`, `severity`, `detail`(한국어 안내)입니다. `error`가 하나도 없으면 `mv_ready: true`.

| 등급 | 코드 |
|---|---|
| error (그대로는 배급 불가) | `TRUNCATED`, `MEDIA_OUTSIDE_MDAT`, `INDEX_INCONSISTENT`, `EXTERNAL_MEDIA`, `NO_VIDEO`, `NO_VIDEO_FRAMES`, `NO_AUDIO`, `NO_AUDIO_FRAMES` |
| warning (플랫폼이 거절하거나 담당자가 봐야 함) | `VIDEO_CODEC`(H.264·HEVC·ProRes 외), `RESOLUTION_BELOW_1080P`(짧은 변), `ROTATED`, `NON_SQUARE_PIXELS`, `INTERLACED`, `VARIABLE_FRAME_RATE`, `FRAME_RATE_UNUSUAL`(23.976·24·25·29.97·30·50·59.94·60 외), `H264_HIGH_BIT_DEPTH`, `VIDEO_BIT_RATE_LOW`(H.264 1080p 8/12Mbps, 1440p 16/24, 2160p 35/53 — 30fps 이하/초과, HEVC는 절반, ProRes 제외), `MULTIPLE_VIDEO`, `MULTIPLE_AUDIO`, `AUDIO_CODEC`(AAC-LC·PCM·ALAC 외), `AUDIO_SAMPLE_RATE`(44.1kHz 미만), `AUDIO_CHANNELS`(스테레오 아님), `AUDIO_BIT_RATE_LOW`(AAC 192kbps 미만), `DURATION_MISMATCH`(1초 초과), `AV_OFFSET`(시작 100ms 초과 어긋남), `DURATION_UNKNOWN` |
| error (소리, `--sound`) | `AUDIO_SILENT`(최대 −80dBFS 미만), `AUDIO_DECODE_ERRORS`(패킷 1% 이상 깨짐; 그 미만은 warning) |
| warning (소리, `--sound`) | `AUDIO_CLIPPING`(클리핑 표본 0.1% 이상, 또는 클리핑·True Peak 0dBTP 초과), `AUDIO_LONG_SILENCE`(무음 30초 이상 또는 50% 이상), `AUDIO_ONE_CHANNEL_SILENT`, `AUDIO_TOO_QUIET`(−30 LUFS 미만) |
| info | `HDR`(PQ·HLG·Dolby Vision), `INDEX_AT_END`, `SOUND_NOT_MEASURED` |

규칙을 바꾸면 `RULE_VERSION`을 올립니다. 백엔드는 보고서의 `rule`을 같이 저장합니다.

### 플랫폼 프로필 (`--profile`)

일반 규칙 위에 한 플랫폼의 사양을 더 봅니다. 결과는 `PROFILE_*` 코드(warning, 음량 안내는 info)로 `issues`에 더해지고,
보고서의 `profile`에 그 사양 버전(`youtube-2026-10` 등)이 남습니다. 일반 규칙과 `mv_ready`는 바뀌지 않습니다.
사양은 `src/check.rs`의 `PROFILES`이고 `video profiles`로 볼 수 있습니다.

| 프로필 | 컨테이너 | 영상 | 소리 | 그 밖 |
|---|---|---|---|---|
| `youtube` | MP4·MOV | H.264·HEVC·ProRes·AV1·VP9, 4:2:0 | AAC-LC·Opus·PCM, 48kHz | 16:9, Fast Start, 편집 목록 없음, −14 LUFS 재생(넘으면 True Peak −1dBTP 이하) |
| `spotify` | MOV·MP4 | H.264·HEVC·ProRes, 4:2:0(ProRes 제외), BT.709·BT.2020 | AAC-LC·PCM 16/24비트, 48·96kHz | 16:9, 영상 1·소리 1 트랙만, −14 LUFS 재생(넘으면 True Peak −2dBTP 이하) |
| `apple` | MOV | ProRes, BT.709 | PCM 16/24비트, 48kHz | 16:9, 영상 1·소리 1 트랙만 |

| 코드 | 뜻 |
|---|---|
| `PROFILE_CONTAINER`, `PROFILE_VIDEO_CODEC`, `PROFILE_AUDIO_CODEC` | 받는 컨테이너·코덱이 아님 |
| `PROFILE_AUDIO_SAMPLE_RATE`, `PROFILE_AUDIO_BIT_DEPTH` | 샘플레이트·PCM 비트 수가 다름 (`convert-audio`로 맞춤) |
| `PROFILE_ASPECT_RATIO`, `PROFILE_CHROMA`, `PROFILE_COLOR` | 16:9가 아님, 4:2:0이 아님, 색 공간이 다름 |
| `PROFILE_INDEX_AT_END`, `PROFILE_EDIT_LIST`, `PROFILE_EXTRA_TRACKS` | 색인이 뒤에 있음, 편집 목록이 트랙을 옮기거나 자름(인코더 지연만 건너뛰는 것은 제외), 영상·소리 외 트랙 |
| `PROFILE_TRUE_PEAK` | 재생 기준보다 큰 마스터의 True Peak가 상한을 넘음 (`--sound`) |
| `PROFILE_LOUDNESS`(info), `PROFILE_LOUDNESS_NOT_MEASURED`(info) | 재생할 때 얼마나 줄어드는지, 소리를 측정하지 않아 음량 기준을 못 봄 |

출처: [YouTube 권장 업로드 인코딩 설정](https://support.google.com/youtube/answer/1722171),
[Spotify 음량 정규화](https://support.spotify.com/artists/article/loudness-normalization), Spotify·Apple 뮤직비디오는 배급사가 전하는
사양(예: [FUGA](https://support.fuga.com/hc/en-us/articles/39331113637012-How-to-Deliver-Music-Videos-to-Spotify-via-FUGA),
[Digdis](https://digdis.helpscoutdocs.com/article/107-requirements-for-spotify-music-videos)). 배급사마다 다른 부분(시작·끝 검은
화면, 인트로 허용 등)은 넣지 않았습니다. 국내 플랫폼 사양은 문서를 받으면 같은 방식으로 더합니다.

## 한계

- 영상 프레임은 디코딩하지 않습니다(깨진 프레임, 검은 화면·레터박스 같은 실제 화면 내용은 보지 않음). 담당자 확인이 필요합니다.
  소리는 `--sound`로 디코드합니다.
- 색인 한도: `moov` 64MiB, `moof` 하나 16MiB, 트랙당 샘플 5천만 개, 최상위 상자 20만 개. 넘으면 `LIMIT`.
- MPEG-4 Part 2·MJPEG·DNxHD 등은 코덱 이름만 알려 줍니다(프로파일·픽셀 형식 없음).

## 측정

전체 결과·재현 방법: [BENCHMARK.md](BENCHMARK.md) (6가지 파일, FFprobe 대비 9~17배 빠르고 메모리 약 1/4).

4분 640×360 H.264+AAC MP4(62MB, 7,200프레임)를 `release-music` 빌드로 30번 실행한 중앙값:

| 도구 | 시간 | 최대 RSS |
|---|---|---|
| `audeniq-music video probe` | 6.0ms | 14.4MiB |
| `ffprobe -show_streams -show_format -of json` (6.1.1) | 64.7ms | 52.3MiB |

퍼징: `fuzz/fuzz_targets/video.rs` ([fuzz/README.md](../../fuzz/README.md)), 테스트의 `corrupted_indexes_do_not_panic`은
색인 바이트를 4,000번 무작위로 바꿔 패닉이 없는지 봅니다.
