# 뮤직비디오 소리 변환 (`audeniq-music video convert-audio`)

배급사가 요구하는 소리 조건(코덱·샘플레이트·채널)으로 MV를 다시 싸는 명령입니다. 지금까지 FFmpeg로 하던

```text
ffmpeg -i IN -c:v copy -c:a aac -b:a 320k -ar 48000 -ac 2 -movflags +faststart OUT.mp4
ffmpeg -i IN -c:v copy -c:a pcm_s24le -ar 48000 -ac 2 OUT.mov
```

를 Rust만으로 합니다(`media-video` + `media-aac` 인코더 + `media-resample`). 영상은 다시 인코딩하지 않습니다.

```text
audeniq-music video convert-audio [--preset NAME] [--codec aac|pcm24] [--bitrate N] [--rate 44100|48000] [--channels L,R|M] IN OUT
```

`--channels`는 원본 채널을 고릅니다. 모든 소리 트랙의 채널을 파일 순서로 1부터 셉니다(트랙 2가 8채널, 트랙 3이 모노면 1–8은 트랙 2, 9는 트랙 3).
두 개면 왼쪽·오른쪽, 하나면 모노(양쪽에 같게). 없으면 아래 **채널 배치** 규칙을 따릅니다.

성공하면 stdout에 결과 JSON, 실패하면 다른 `video` 명령과 같이 stderr에 `{"detail":…,"error":"IO|INVALID|LIMIT|USAGE"}`, 종료 코드 2.

## 프리셋

| 이름 | 컨테이너 | 소리 |
|---|---|---|
| `aac-320` (`.mp4` 기본) | MP4 | AAC-LC 320 kbit/s, 48 kHz 스테레오 |
| `youtube` | MP4 | AAC-LC 384 kbit/s, 48 kHz 스테레오 (YouTube 권장 업로드 사양) |
| `apple-music` | MOV | 24비트 PCM(`lpcm`), 48 kHz 스테레오 (ProRes + LPCM 납품) |
| `pcm24` (`.mov` 기본) | MOV | 24비트 PCM, 48 kHz 스테레오 |

컨테이너는 OUT 확장자로 정합니다(`.mov` → MOV, 그 밖 → MP4). `--codec`·`--bitrate`·`--rate`로 프리셋 값을 바꿀 수 있습니다.
배급사 사양은 바뀔 수 있으니 실제 납품 전에 각 사의 최신 문서와 맞춰 보세요.

## 하는 일

1. **영상은 그대로.** 영상·타임코드(`tmcd`)·자막 트랙의 샘플 바이트와 표(`stsd`, `stts`, `ctts`, `stss`, `sdtp`, `edts` …)를
   그대로 옮기고 `stsc`와 청크 위치(`co64`)만 새로 씁니다. FFmpeg `framemd5`로 비교하면 원본과 같습니다.
2. **소리는 새로.** 배급 소리를 디코드합니다([README.md](README.md)의 **채널 배치**와 같은 규칙: 첫 번째로 켜진 소리 트랙,
   모노 트랙 두 개면 L/R로, 스피커 배치 없는 다채널이면 1·2채널, 또는 `--channels`).
   디코더: AAC-LC·HE-AAC·HE-AACv2(`media-aac`), MP3(`media-mp3`), ALAC(`audeniq-qc`), PCM(정수·부동소수, LE/BE)은 직접.
   트랙마다 원본 편집 목록을 따릅니다(앞쪽 빈 편집 → 무음, `media_time` 만큼 인코더 지연 제거, 표시 길이에서 자름).
3. 스피커 배치를 아는 5.1/7.1과 모노는 스테레오로(ITU-R BS.775: C·서라운드 −3 dB, LFE 제외), 샘플레이트가 다르면 192탭 Kaiser sinc로 변환(44.1→48 kHz 90 dB 이상).
4. AAC-LC로 인코딩(인코더 지연 1024는 `elst media_time`로 숨김) 하거나 24비트 PCM으로 씁니다. 다른 소리 트랙은 뺍니다.
5. `ftyp`, `moov`, `mdat` 순(fast start)으로, 트랙들을 0.5초 청크로 섞어 씁니다.

조각 MP4(`moof`) 입력과 MP4 안의 ProRes 같은 조합은 `LIMIT`으로 거절합니다(ProRes는 `.mov`로).
Opus·FLAC·AC-3/E-AC-3·Vorbis 소리, 3채널 이상 ALAC, MKV·MPEG-TS는 `INVALID`로 거절합니다(원본을 MP4/MOV에 AAC 또는 PCM으로 다시 받기).

## 결과 JSON

```json
{"container":"mp4","source_sound":{"track_id":2,"codec_name":"aac","sample_rate":44100,"channels":2},
 "mapping":"stereo","source_channels":[[2,1],[2,2]],"codec_name":"aac","sample_rate":48000,"channels":2,"bit_rate":320000,"frames":384000,"duration":8.0,
 "copied_tracks":[1],"dropped_tracks":[],"over_full_scale":0,"bytes":3232104}
```

`mapping`은 채널 배치(`mono`·`stereo`·`downmix`·`dual-mono`·`channels`), `source_channels`는 쓴 원본 채널의 [트랙 id, 채널 번호]입니다.
합친 트랙(듀얼 모노의 두 번째 트랙)은 `dropped_tracks`에 넣지 않습니다.
`over_full_scale`은 다운믹스·리샘플 뒤 ±1.0을 넘은 샘플 수입니다(PCM은 잘리고, AAC는 그대로 실림). 0이 아니면 마스터 레벨을 확인하세요.
변환 결과는 `video probe --sound`로 다시 검사할 수 있습니다.

## 검증 (`crates/media-video/tests/convert.rs`, ffmpeg가 있으면 실행)

- H.264+AAC 44.1 kHz → AAC 48 kHz MP4: 영상 `framemd5` 동일, FFprobe가 aac/48000/2ch, 길이 4.0초, 시작 0, `moov`가 `mdat` 앞, 톤 위치·크기 일치
- ProRes+PCM 5.1+타임코드 MOV → 24비트 PCM MOV: 영상 동일, 타임코드 `01:00:00:00` 유지, 다운믹스가 식과 1e-5 안에서 같음, ProRes→MP4는 거절
- 소리 없는 파일은 오류
- 모노 트랙 두 개 → 왼쪽·오른쪽이 각 트랙과 표본 단위로 같음, `--channels 2,1`로 바꿔 넣기, 측정·`sound-ranges`도 두 트랙
- 디스크리트 4채널 PCM(`chan` DiscreteInOrder) → 1·2채널 그대로, `--channels 3`은 3채널 모노, 없는 채널은 오류
- 24비트 ALAC → PCM이 FFmpeg 디코드와 같음, MP3-in-MP4 → PCM이 FFmpeg 디코드(편집 목록 포함)와 같음

## 성능 (3분 1080p H.264 8 Mbit/s + AAC 44.1 kHz, 185 MB, 4코어 컨테이너, 두 번 중 빠른 값)

| | 시간 | CPU | 최대 메모리 |
|---|---|---|---|
| `convert-audio --preset aac-320` | 9.9 s | 9.6 s | 55 MB |
| `ffmpeg … -c:a aac -b:a 320k` | 23.8 s | 23.6 s | 58 MB |
| `convert-audio --preset pcm24` | 2.8 s | 2.7 s | 104 MB |
| `ffmpeg … -c:a pcm_s24le` | 1.3 s | 1.5 s | 56 MB |

PCM에서 FFmpeg가 빠른 것은 기본 리샘플러 필터가 훨씬 짧기 때문입니다(우리는 192탭, 통과대역 20 kHz까지 평탄).
PCM 변환은 결과 소리를 메모리에 모았다가 쓰므로 길이에 비례해 메모리가 늡니다(24비트 스테레오 48 kHz: 분당 17 MB).

AAC 인코더 품질(같은 신호, FFmpeg 디코드로 원본과 비교한 SNR):

| | 128k | 192k | 256k | 320k |
|---|---|---|---|---|
| `media-aac` | 18.9 dB | 22.9 dB | 27.2 dB | 30.7 dB |
| FFmpeg `aac` | 21.6 dB | 22.6 dB | 22.8 dB | 23.3 dB |

배급용 256k 이상에서 앞서고 16 kHz 위 대역도 남깁니다. 타격음 앞 20 ms의 프리에코는 FFmpeg보다 약 25 dB 낮습니다.
128k처럼 낮은 비트레이트는 전 대역을 남기느라 FFmpeg보다 SNR이 낮습니다(배급 프리셋에는 쓰지 않음).
