#!/usr/bin/env python3
"""Downloads a representative GTSinger subset for `tune-bench gtsinger` (stdlib only).

GTSinger (Zhang et al., NeurIPS 2024; https://huggingface.co/datasets/GTSinger/GTSinger) is CC BY-NC-SA 4.0:
benchmarking is fine, the audio must not be committed or redistributed with WAVE. Read the licence first.

What is fetched (English part, .wav + .json annotation per segment):
  * for each singer and technique in TECHNIQUES, up to --songs songs and --segments segment indices that exist in
    both `Control_Group` (sung plainly) and the technique group (the same phrase with vibrato / glissando):
    the same singer singing the same notes with and without the ornament;
  * every segment of the songs two singers (a tenor and an alto) both recorded: a real cross-singer, cross-range pair.

Usage:  python fetch_gtsinger.py --out D:/wave-datasets/gtsinger [--songs 3] [--segments 4]
"""
import argparse
import collections
import json
import os
import sys
import urllib.parse
import urllib.request

API = "https://huggingface.co/api/datasets/GTSinger/GTSinger"
RESOLVE = "https://huggingface.co/datasets/GTSinger/GTSinger/resolve/main/"
TECHNIQUES = {"Vibrato": "Vibrato_Group", "Glissando": "Glissando_Group"}


def listing():
    with urllib.request.urlopen(API, timeout=60) as r:
        d = json.load(r)
    return [s["rfilename"] for s in d.get("siblings", [])]


def fetch(rel, out):
    dst = os.path.join(out, *rel.split("/"))
    if os.path.exists(dst) and os.path.getsize(dst) > 0:
        return dst, 0
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    url = RESOLVE + urllib.parse.quote(rel)
    tmp = dst + ".part"
    with urllib.request.urlopen(url, timeout=120) as r, open(tmp, "wb") as f:
        n = 0
        while True:
            b = r.read(1 << 16)
            if not b:
                break
            f.write(b)
            n += len(b)
    os.replace(tmp, dst)
    return dst, n


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--songs", type=int, default=3)
    ap.add_argument("--segments", type=int, default=4)
    ap.add_argument("--cross-segments", type=int, default=12)
    a = ap.parse_args()
    files = [f for f in listing() if f.startswith("English/") and (f.endswith(".wav") or f.endswith(".json"))]
    have = set(files)
    # (singer, technique, song) -> group -> segment ids
    seg = collections.defaultdict(lambda: collections.defaultdict(set))
    for f in files:
        p = f.split("/")
        if len(p) != 6 or not p[5].endswith(".wav"):
            continue
        seg[(p[1], p[2], p[3])][p[4]].add(p[5][:-4])
    want = []
    for tech, group in TECHNIQUES.items():
        by_singer = collections.defaultdict(list)
        for (singer, t, song), groups in sorted(seg.items()):
            if t == tech and group in groups and "Control_Group" in groups:
                by_singer[singer].append((song, sorted(groups[group] & groups["Control_Group"])))
        for singer, songs in sorted(by_singer.items()):
            for song, ids in songs[: a.songs]:
                for i in ids[: a.segments]:
                    for g in ("Control_Group", group):
                        want += [f"English/{singer}/{tech}/{song}/{g}/{i}.wav", f"English/{singer}/{tech}/{song}/{g}/{i}.json"]
    # songs recorded by two singers: their Control_Group segments
    singers_of = collections.defaultdict(set)
    for (singer, t, song), groups in seg.items():
        if "Control_Group" in groups:
            singers_of[(t, song)].add(singer)
    for (t, song), who in sorted(singers_of.items()):
        if len(who) < 2 or t not in TECHNIQUES:
            continue
        for singer in sorted(who):
            for i in sorted(seg[(singer, t, song)]["Control_Group"])[: a.cross_segments]:
                want += [f"English/{singer}/{t}/{song}/Control_Group/{i}.wav", f"English/{singer}/{t}/{song}/Control_Group/{i}.json"]
    want = [w for w in dict.fromkeys(want) if w in have]
    total = 0
    for k, rel in enumerate(want):
        try:
            _, n = fetch(rel, a.out)
            total += n
        except Exception as e:  # keep going; the benchmark reports missing files as failures
            print(f"failed: {rel}: {e}", file=sys.stderr)
        if k % 50 == 0:
            print(f"{k}/{len(want)} files, {total / 1e6:.1f} MB", file=sys.stderr)
    with open(os.path.join(a.out, "SUBSET.txt"), "w", encoding="utf-8") as f:
        f.write("GTSinger subset (CC BY-NC-SA 4.0, https://huggingface.co/datasets/GTSinger/GTSinger), fetched by fetch_gtsinger.py\n")
        f.write("\n".join(want) + "\n")
    print(json.dumps({"files": len(want), "downloadedMB": round(total / 1e6, 1), "out": a.out}))


if __name__ == "__main__":
    main()
