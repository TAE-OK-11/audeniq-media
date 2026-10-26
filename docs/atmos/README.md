# Dolby Atmos 마스터 분석 (`media-adm`, `audeniq-music atmos`)

애플 뮤직 등 공간 음향 배급에 쓰는 Dolby Atmos 마스터(ADM BWF)를 읽고 납품 규격을 검사합니다.
외부 라이브러리 없이 `media-json`만 씁니다(XML 파서도 자체).

```text
audeniq-music atmos probe FILE           보고서 JSON (헤더·ADM만 읽음, 수십 ms)
audeniq-music atmos probe --peaks FILE   + PCM 전체를 한 번 읽어 트랙별 피크·무음 트랙·0dBFS 표본
audeniq-music atmos capabilities         엔진·규칙 버전
```

오류는 `qc`·`video`와 같은 약속입니다: stderr에 `{"detail":…,"error":"IO|INVALID|LIMIT|USAGE"}`, 종료 코드 2.

## 무엇을 읽나

| 영역 | 내용 |
|---|---|
| 컨테이너 | RIFF, RF64, BW64(ITU-R BS.2088): `ds64`의 64비트 크기(데이터·표), 홀수 크기 패딩 |
| 형식 | `fmt `(PCM·float, WAVE_FORMAT_EXTENSIBLE의 하위 형식·유효 비트·채널 마스크) |
| ADM | `chna`(트랙 번호 ↔ audioTrackUID ↔ 트랙/팩 형식), `axml`(ITU-R BS.2076: audioProgramme·audioContent·audioObject·audioPackFormat·audioChannelFormat·audioTrackUID, loudnessMetadata) |
| 그 밖 | `bext` v2 음량(EBU Tech 3285), `dbmd`(Dolby 메타데이터) 유무 |
| PCM (`--peaks`) | 16/24/32비트 정수 PCM을 4MiB씩 읽어 트랙별 최대값, −90dBFS 미만 무음 트랙, 최대치 표본 |

ADM XML은 자체 풀 파서로 한 번 훑으며 필요한 것만 모읍니다(블록 형식은 개수만). DTD·외부 엔티티는 해석하지 않고,
중첩 256단계·요소 2천만 개·메타데이터 청크 256MiB가 한도입니다. 베드 구성은 DirectSpeakers 팩의 스피커 라벨
(`RC_L`…, `M+030`…, `U+045`…)로 `7.1.2`처럼 셉니다.

**음량은 마스터가 적어 둔 값(`loudnessMetadata` 또는 `bext`)입니다.** 렌더링해서 재지는 않습니다.

## 납품 검사 (`atmos-delivery-v1`)

`issues`의 각 항목은 `code`, `severity`, `detail`(한국어). `error`가 없으면 `deliverable: true`.

| 등급 | 코드 |
|---|---|
| error | `NO_FORMAT`, `NO_AUDIO_DATA`, `NOT_INTEGER_PCM`, `BIT_DEPTH`(24비트 아님), `SAMPLE_RATE`(48/96kHz 아님), `TOO_MANY_TRACKS`(128 초과), `TRUNCATED`, `NO_ADM`(axml·chna 없음), `ADM_INVALID`, `CHNA_INVALID`, `CHNA_TRACK_RANGE`, `CHNA_UID_UNKNOWN`, `ADM_TRACK_MISSING`, `NO_PROGRAMME`, `TOO_MANY_OBJECTS`(118 초과), `ALL_SILENT`(`--peaks`) |
| warning | `SAMPLE_RATE_96K`, `DATA_ALIGNMENT`, `UNUSED_TRACKS`, `MULTIPLE_PROGRAMMES`, `NO_BED`, `BED_LAYOUT`(7.1.2 아님), `DURATION_MISMATCH`(1초 초과), `LOUDNESS_HIGH`(−18 LKFS 초과), `TRUE_PEAK_HIGH`(−1 dBTP 초과), `CLIPPING`(`--peaks`) |
| info | `LOUDNESS_MISSING`, `NO_DOLBY_METADATA`, `SILENT_TRACKS`(`--peaks`) |

## 검증

- `crates/media-adm/tests/atmos.rs`: 렌더러가 만드는 모양(BW64+ds64, 7.1.2 베드 팩, 오브젝트 팩, chna, axml, dbmd)으로 만든
  마스터와 RIFF·RF64 변형, 규격 위반(chna 없음, 16비트, 44.1kHz, 오브젝트 119개, 알 수 없는 UID, 음량 초과, 96kHz, 클리핑),
  끊긴 파일, 무작위 손상 3,000회(패닉 없음). FFprobe가 같은 형식(pcm_s24le, 48000, 12)으로 읽는지.
- EBU ADM Renderer의 공개 시험 파일 `test_bwf.wav`(4트랙, 오브젝트 5개)를 로컬에서 읽어 확인(저장소에는 넣지 않음).
- 퍼징 대상 `fuzz/fuzz_targets/adm.rs`.

## 측정

12트랙 24비트 48kHz 5분 BW64(518MB), `release-music`: 헤더·ADM만 30ms, `--peaks` 1.6초(약 330MB/s).

## 아직 없는 것

- 백엔드 업로드 경로: Atmos 마스터를 받는 화면·저장·검사 연결은 아직 없습니다(엔진만).
- 렌더링 기반 음량(5.1/바이노럴 다운믹스 측정), Dolby `dbmd` 세부 해석.
