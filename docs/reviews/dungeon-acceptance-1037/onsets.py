"""Measure exported mixer WAVs against their timestamp manifests.

For each WAV with an adjacent CSV: peak, the level rise at every manifested cue start,
and energy onsets that no manifested start explains. Standard library only.
Measuring a signal is not listening to it.
"""
import array, csv, math, sys, wave
from pathlib import Path

WINDOW = 0.010  # seconds per analysis frame
LEAD = 0.040  # look this long before and after a manifested start
EXPLAINED = 0.080  # an onset this soon after a manifested start belongs to it
FLOOR_DB = -50.0  # onsets quieter than this are ignored
JUMP_DB = 9.0  # a frame this much louder than the previous 50 ms is an onset


def read(path):
    with wave.open(str(path)) as w:
        assert w.getsampwidth() == 2, path
        rate, channels = w.getframerate(), w.getnchannels()
        data = array.array("h", w.readframes(w.getnframes()))
    mono = [max(abs(data[i]), abs(data[i + 1])) / 32768.0 for i in range(0, len(data) - channels + 1, channels)]
    return rate, mono


def db(x):
    return 20 * math.log10(x) if x > 0 else -math.inf


def rms(samples):
    return math.sqrt(sum(s * s for s in samples) / len(samples)) if samples else 0.0


def starts(csv_path):
    with open(csv_path) as f:
        rows = list(csv.DictReader(f))
    key = "seconds" if rows and "seconds" in rows[0] else "start_seconds"
    return [(float(r[key]), r["cue"]) for r in rows]


def analyse(wav_path):
    rate, mono = read(wav_path)
    cues = starts(wav_path.with_suffix(".csv"))
    peak = max(mono) if mono else 0.0
    n = int(WINDOW * rate)
    frames = [rms(mono[i : i + n]) for i in range(0, len(mono), n)]
    rises = []
    for t, cue in cues:
        i = int(t * rate)
        before = rms(mono[max(0, i - int(LEAD * rate)) : i])
        after = rms(mono[i : i + int(LEAD * rate)])
        rises.append((t, cue, db(after) - db(before) if before > 0 else math.inf, db(after)))
    onsets = []
    for k in range(5, len(frames)):
        prior = max(frames[k - 5 : k])
        if db(frames[k]) > FLOOR_DB and db(frames[k]) - db(prior) >= JUMP_DB:
            onsets.append(k * WINDOW)
    unexplained = [o for o in onsets if not any(0 <= o - t <= EXPLAINED or 0 <= t - o <= WINDOW for t, _ in cues)]
    return {
        "name": wav_path.stem,
        "seconds": len(mono) / rate,
        "peak": peak,
        "cues": len(cues),
        "silent_cues": [(t, c) for t, c, _, level in rises if level < FLOOR_DB],
        "min_rise_db": min((r for _, _, r, _ in rises), default=math.nan),
        "onsets": len(onsets),
        "unexplained": unexplained,
    }


def main(directory):
    print("take,seconds,peak,manifest_cues,cues_below_floor,min_rise_db,onsets,unexplained_onsets")
    for wav_path in sorted(Path(directory).glob("*.wav")):
        if not wav_path.with_suffix(".csv").exists():
            continue
        r = analyse(wav_path)
        print(
            f"{r['name']},{r['seconds']:.2f},{r['peak']:.4f},{r['cues']},{len(r['silent_cues'])},"
            f"{r['min_rise_db']:.1f},{r['onsets']},{';'.join(f'{o:.2f}' for o in r['unexplained'])}"
        )


if __name__ == "__main__":
    main(sys.argv[1])
