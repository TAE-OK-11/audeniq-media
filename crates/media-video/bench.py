#!/usr/bin/env python3
"""`audeniq-music video probe` vs `ffprobe -show_streams -show_format` on the
given files: median wall time, CPU (user+sys) and peak RSS of the child
process over N runs (warm page cache), plus how many ranged reads the backend
makes to fetch the same index from object storage (crates/core/src/mp4.rs
`index`: box headers and ftyp/moov/moof payloads through an 8 KiB window).

    python3 -I crates/media-video/bench.py target/release-music/audeniq-music FILE... [--runs 30]
"""
import os, struct, subprocess, sys, time


def run(cmd):
    t = time.perf_counter()
    p = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    _, status, ru = os.wait4(p.pid, 0)
    wall = time.perf_counter() - t
    if os.waitstatus_to_exitcode(status) != 0:
        sys.exit(f"failed: {' '.join(cmd)}")
    return wall, ru.ru_utime + ru.ru_stime, ru.ru_maxrss


WINDOW = 8 << 10


def ranged_reads(path):
    """Requests and bytes of the backend's index walk (mirrors mp4::index:
    reads go through one 8 KiB window; ftyp/moov/moof payloads are kept)."""
    size = os.path.getsize(path)
    reads = nbytes = at = 0
    buf_at, buf = 0, b""
    with open(path, "rb") as f:

        def read(pos, n):
            nonlocal reads, nbytes, buf_at, buf
            have = buf_at + len(buf)
            if pos >= buf_at and (pos + n <= have or (have < buf_at + WINDOW and pos < have)):
                return buf[pos - buf_at : pos - buf_at + n]
            f.seek(pos)
            buf_at, buf = pos, f.read(max(n, WINDOW))
            reads, nbytes = reads + 1, nbytes + len(buf)
            return buf[:n]

        while at + 8 <= size:
            head = read(at, 16)
            n, typ = struct.unpack(">I4s", head[:8])
            hdr = 8
            if n == 1:
                n, hdr = struct.unpack(">Q", head[8:16])[0], 16
            elif n == 0:
                n = size - at
            if n < hdr or at + n > size:
                break
            if typ in (b"moov", b"ftyp", b"moof"):
                read(at + hdr, n - hdr)
            at += n
    return reads, nbytes


def median(xs):
    xs = sorted(xs)
    return xs[len(xs) // 2]


def main():
    args = sys.argv[1:]
    runs = 30
    if "--runs" in args:
        i = args.index("--runs")
        runs = int(args[i + 1])
        del args[i : i + 2]
    engine, files = args[0], args[1:]
    print("| 파일 | 크기 | 도구 | 시간(중앙값) | CPU | 최대 RSS | 백엔드 범위 읽기 |")
    print("|---|---|---|---|---|---|---|")
    for path in files:
        name = os.path.basename(path)
        size = os.path.getsize(path) / 1e6
        reads, nbytes = ranged_reads(path)
        tools = [
            ("audeniq-music video probe", [engine, "video", "probe", path]),
            ("ffprobe", ["ffprobe", "-v", "error", "-show_streams", "-show_format", "-of", "json", path]),
        ]
        results = []
        for label, cmd in tools:
            run(cmd)  # warm the page cache
            samples = [run(cmd) for _ in range(runs)]
            wall = median([s[0] for s in samples]) * 1000
            cpu = median([s[1] for s in samples]) * 1000
            rss = max(s[2] for s in samples) / 1024
            results.append(wall)
            io = f"{reads}회, {nbytes / 1e6:.2f}MB" if label.startswith("audeniq") else ""
            print(f"| {name} | {size:.0f}MB | {label} | {wall:.1f}ms | {cpu:.1f}ms | {rss:.1f}MiB | {io} |")
        print(f"| | | 배수 | ×{results[1] / results[0]:.1f} | | | |")


if __name__ == "__main__":
    main()
