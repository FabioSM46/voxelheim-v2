#!/usr/bin/env python3
"""Verify and index opt-in castle capture artifacts, without publishing paths."""
import argparse
import hashlib
import json
import math
from pathlib import Path
import struct
import subprocess


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fields(path):
    result = {}
    for line in path.read_text().splitlines():
        key, value = line.split("=", 1)
        if key in result:
            raise ValueError("duplicate capture metadata key")
        result[key] = value
    return result


def trace_points(path):
    lines = path.read_text().splitlines()
    if not lines or lines[0] != "tick\tlocal_feet_x\tlocal_feet_y\tlocal_feet_z":
        raise ValueError("invalid trace header")
    points = []
    for tick, line in enumerate(lines[1:]):
        values = line.split("\t")
        if len(values) != 4 or int(values[0]) != tick:
            raise ValueError("trace ticks are not contiguous")
        point = [float(v) for v in values[1:]]
        if not all(math.isfinite(v) for v in point):
            raise ValueError("trace contains a non-finite position")
        points.append(point)
    if not points:
        raise ValueError("empty trace")
    # Columns are keep-local: fixture.building_origin is applied by the renderer.
    # Comparing these to a world-space gate would incorrectly reject other seeds.
    gate = (31.5, 0.0, 62.5)
    for endpoint in (points[0], points[-1]):
        if any(not math.isclose(a, b, abs_tol=0.02) for a, b in zip(endpoint, gate)):
            raise ValueError("trace endpoints do not match the canonical gate")
    return points


def fixture_info(path):
    data = path.read_bytes()
    if len(data) < 96 or data[:8] != b"VHCAST03":
        raise ValueError("invalid fixture header")
    version, worldgen, seed, actual, turn, mode = struct.unpack_from("<IIqIII", data, 8)
    if version != 3 or actual > 3 or turn > 3 or mode > 1:
        raise ValueError("unsupported fixture")
    origin = struct.unpack_from("<qqq", data, 36)
    volume_origin = struct.unpack_from("<qqq", data, 60)
    size = struct.unpack_from("<III", data, 84)
    if len(data) != 96 + 2 * math.prod(size):
        raise ValueError("fixture payload length mismatch")
    metadata = json.loads(Path(str(path) + ".json").read_text())
    digest = sha256(path)
    if metadata["sha256"] != digest:
        raise ValueError("fixture differs from server export digest")
    return dict(name=path.name, sha256=digest, source_commit=metadata["source_commit"],
                worldgen=worldgen, seed=seed, actual_facing=actual, review_turn=turn,
                scene_mode=mode, building_origin=origin, volume_origin=volume_origin,
                dimensions=size)


def manifest(directory):
    fixtures = [fixture_info(p) for p in sorted(directory.glob("*.vhc"))]
    if not fixtures:
        raise ValueError("no server fixtures")
    captures = []
    for report in sorted(directory.glob("*.txt")):
        data = fields(report)
        if data.get("source_dirty") != "false":
            raise ValueError("capture source must be committed and clean")
        matches = [f for f in fixtures if all(
            str(f[k]) == data[k] for k in
            ("worldgen", "seed", "actual_facing", "review_turn", "scene_mode"))]
        if len(matches) != 1 or matches[0]["source_commit"] != data["source_commit"]:
            raise ValueError("capture and fixture source/frame do not match uniquely")
        item = dict(report=report.name, settings=data, fixture=matches[0]["name"])
        if "trace_file" in data:
            name = data["trace_file"]
            if Path(name).name != name:
                raise ValueError("trace reference must be a basename")
            trace = directory / "traces" / name
            points = trace_points(trace)
            indices = list(range(0, len(points), 2))
            if indices[-1] != len(points) - 1:
                indices.append(len(points) - 1)
            frame_dir = report.with_suffix(".frames")
            frames = sorted(frame_dir.glob("frame-*.png"))
            if [p.name for p in frames] != [f"frame-{i:05}.png" for i in range(len(indices))]:
                raise ValueError("capture frame sequence is incomplete")
            video = report.with_suffix(".mp4")
            probe = json.loads(subprocess.check_output([
                "ffprobe", "-v", "error", "-show_entries", "stream=width,height,nb_frames",
                "-show_entries", "format=duration", "-of", "json", str(video)]))
            stream = probe["streams"][0]
            if int(stream["nb_frames"]) != len(indices) or (stream["width"], stream["height"]) != (1280, 720):
                raise ValueError("encoded video differs from captured sequence")
            expected_seconds = 1 + (len(points) - 1) / 20
            if not math.isclose(float(data["capture_elapsed_seconds"]), expected_seconds, abs_tol=1e-6):
                raise ValueError("capture time differs from authoritative trace duration")
            item.update(artifact=video.name, sha256=sha256(video), frames=len(indices),
                        duration_seconds=float(probe["format"]["duration"]),
                        trace=dict(name=name, sha256=sha256(trace), ticks=len(points),
                                   first_feet=points[0], final_feet=points[-1],
                                   highest_feet=max(p[1] for p in points)))
        else:
            png = report.with_suffix(".png")
            header = png.read_bytes()[:24]
            if header[:8] != b"\x89PNG\r\n\x1a\n" or struct.unpack_from(">II", header, 16) != (1280, 720):
                raise ValueError("invalid capture PNG dimensions")
            item.update(artifact=png.name, sha256=sha256(png))
        captures.append(item)
    if not captures:
        raise ValueError("no completed captures")
    return dict(format=1, scope="Bounded generated region or labelled isolated review turn; not full-game city FPS",
                fixtures=fixtures, captures=captures)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path, help="Fresh capture output directory")
    args = parser.parse_args()
    result = manifest(args.directory)
    destination = args.directory / "manifest.json"
    destination.write_text(json.dumps(result, indent=2) + "\n")
    print(f"Verified {len(result['captures'])} captures; wrote {destination.name}")


if __name__ == "__main__":
    main()
