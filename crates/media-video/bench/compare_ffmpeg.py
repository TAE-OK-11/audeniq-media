#!/usr/bin/env python3
"""The MV sound tools against FFmpeg on the same machine.

Makes a corpus of music videos (1080p H.264 with AAC-LC, MP3 or 24-bit PCM
sound, and HE-AAC / HE-AACv2 from FFmpeg's conformance samples when they can
be fetched), then times probe, decode, loudness measurement and both
conversion presets against the FFmpeg command doing the same job. Prints
the median wall time, CPU time (user + system) and peak RSS of `--runs`
runs and writes them to `--json`.

    cargo build --profile release-music -p audeniq-music
    cargo build --profile release-music -p media-video --example decode_sound
    python3 crates/media-video/bench/compare_ffmpeg.py --seconds 240
"""
import argparse, json, os, statistics, subprocess, sys, urllib.request

ap = argparse.ArgumentParser()
ap.add_argument('--target', default='target/release-music')
ap.add_argument('--corpus', default='target/mv-bench')
ap.add_argument('--seconds', type=int, default=240)
ap.add_argument('--runs', type=int, default=3)
ap.add_argument('--json', default='mv-benchmark.json')
ap.add_argument('only', nargs='*', help='run cases whose label contains one of these')
args = ap.parse_args()

B = os.path.join(args.target, 'audeniq-music')
D = os.path.join(args.target, 'examples', 'decode_sound')
C = args.corpus
OUT = os.path.join(C, 'out')
os.makedirs(OUT, exist_ok=True)
F = ['ffmpeg', '-nostdin', '-hide_banner', '-loglevel', 'error', '-y']
FATE = 'https://fate-suite.ffmpeg.org/aac/'


def sh(cmd):
    subprocess.run(cmd, check=True)


def corpus():
    """The test files, made once."""
    p = lambda n: os.path.join(C, n)
    t = str(args.seconds)
    if not os.path.exists(p('m441.wav')):
        music = ('[0][1]amix=inputs=2:normalize=0,pan=stereo|c0=c0|c1=0.85*c0+0.1*c1,'
                 'volume=0.9[a]')
        tone = ('aevalsrc=0.2*sin(2*PI*261.6*t)+0.15*sin(2*PI*329.6*t)+0.12*sin(2*PI*392*t)'
                '+0.5*exp(-30*mod(t\\,0.5))*sin(2*PI*80*t)'
                '+0.3*exp(-200*mod(t+0.25\\,0.5))*(random(0)-0.5):s=44100')
        sh(F + ['-f', 'lavfi', '-i', 'anoisesrc=c=pink:r=44100:a=0.12', '-f', 'lavfi', '-i', tone,
                '-filter_complex', music, '-map', '[a]', '-t', t, '-c:a', 'pcm_s24le', p('m441.wav')])
    if not os.path.exists(p('m48.wav')):
        sh(F + ['-i', p('m441.wav'), '-ar', '48000', '-c:a', 'pcm_s24le', p('m48.wav')])
    if not os.path.exists(p('v1080.mp4')):
        sh(F + ['-f', 'lavfi', '-i', 'testsrc2=size=1920x1080:rate=24000/1001', '-t', t,
                '-c:v', 'libx264', '-preset', 'ultrafast', '-b:v', '8M', '-maxrate', '8M',
                '-bufsize', '16M', '-bf', '2', p('v1080.mp4')])
    files = []
    for name, wav, codec in [('mv_aac.mp4', 'm441.wav', ['-c:a', 'aac', '-b:a', '256k']),
                             ('mv_mp3.mp4', 'm441.wav', ['-c:a', 'libmp3lame', '-b:a', '320k']),
                             ('mv_pcm48.mov', 'm48.wav', ['-c:a', 'pcm_s24le'])]:
        if not os.path.exists(p(name)):
            sh(F + ['-i', p('v1080.mp4'), '-i', p(wav), '-map', '0:v', '-map', '1:a',
                    '-c:v', 'copy'] + codec + [p(name)])
        files.append((name, True))
    # HE-AAC and HE-AACv2: conformance streams joined to about the same length.
    for name, sample, length in [('mv_he.mp4', 'al_sbr_cm_48_2.mp4', 32.2),
                                 ('mv_ps.mp4', 'al_sbr_ps_06_new.mp4', 9.0)]:
        if not os.path.exists(p(name)):
            try:
                if not os.path.exists(p(sample)):
                    urllib.request.urlretrieve(FATE + sample, p(sample))
            except OSError as e:
                print(f'skip {name}: {e}', file=sys.stderr)
                continue
            with open(p(name + '.txt'), 'w') as f:
                for _ in range(max(1, round(args.seconds / length))):
                    f.write(f"file '{os.path.abspath(p(sample))}'\n")
            sh(F + ['-f', 'concat', '-safe', '0', '-i', p(name + '.txt'), '-c', 'copy', p(name)])
        files.append((name, False))
    return files


def helper():
    """`runrss`, built next to the corpus."""
    exe = os.path.join(C, 'runrss')
    src = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'runrss.c')
    if not os.path.exists(exe) or os.path.getmtime(exe) < os.path.getmtime(src):
        sh(['cc', '-O2', '-o', exe, src])
    return exe


def run(cmd):
    r, w = os.pipe()
    proc = subprocess.Popen([RUNRSS, str(w)] + cmd, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL, pass_fds=(w,))
    os.close(w)
    line = os.read(r, 256).decode()
    os.close(r)
    if proc.wait() != 0:
        raise SystemExit(f'failed: {" ".join(cmd)}')
    wall, cpu, rss = line.split()
    return float(wall), float(cpu), int(rss) / 1024


def bench(cmd):
    rs = [run(cmd) for _ in range(args.runs)]
    for f in os.listdir(OUT):
        os.remove(os.path.join(OUT, f))
    return [statistics.median(x) for x in zip(*rs)]


RUNRSS = helper()
cases = []
for name, convert in corpus():
    f = os.path.join(C, name)
    cases += [
        (f'probe {name}', [B, 'video', 'probe', f],
         ['ffprobe', '-v', 'error', '-show_streams', '-show_format', '-of', 'json', f]),
        (f'decode {name}', [D, '--null', f],
         F + ['-i', f, '-map', '0:a:0', '-f', 'f32le', '/dev/null']),
        (f'measure {name}', [B, 'video', 'probe', '--sound', f],
         F + ['-nostats', '-i', f, '-map', '0:a:0', '-af', 'ebur128=peak=true', '-f', 'null', '-']),
    ]
    if convert:
        cases += [
            (f'aac-320 {name}',
             [B, 'video', 'convert-audio', '--preset', 'aac-320', f, os.path.join(OUT, 'o.mp4')],
             F + ['-i', f, '-c:v', 'copy', '-c:a', 'aac', '-b:a', '320k', '-ar', '48000', '-ac', '2',
                  '-movflags', '+faststart', os.path.join(OUT, 'o.mp4')]),
            (f'pcm24 {name}',
             [B, 'video', 'convert-audio', '--preset', 'pcm24', f, os.path.join(OUT, 'o.mov')],
             F + ['-i', f, '-c:v', 'copy', '-c:a', 'pcm_s24le', '-ar', '48000', '-ac', '2',
                  os.path.join(OUT, 'o.mov')]),
        ]

rows = []
print(f'{"case":24} {"ours s":>7} {"cpu s":>7} {"MB":>6} | {"ffmpeg s":>8} {"cpu s":>7} {"MB":>6}'
      ' | ×wall  ×cpu  ×mem')
for label, ours, ff in cases:
    if args.only and not any(o in label for o in args.only):
        continue
    o, g = bench(ours), bench(ff)
    rows.append({'case': label, 'ours': o, 'ffmpeg': g})
    print(f'{label:24} {o[0]:7.3f} {o[1]:7.3f} {o[2]:6.1f} | {g[0]:8.3f} {g[1]:7.3f} {g[2]:6.1f}'
          f' | {g[0] / o[0]:5.2f} {g[1] / o[1]:5.2f} {g[2] / o[2]:5.2f}', flush=True)
with open(args.json, 'w') as f:
    json.dump({'machine': os.uname().machine, 'runs': args.runs, 'rows': rows}, f, indent=1)
