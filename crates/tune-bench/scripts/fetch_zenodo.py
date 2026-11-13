#!/usr/bin/env python3
"""Downloads and unpacks the openly licensed Zenodo datasets the tune-bench suites use (stdlib only).

  vocalset  Annotated VocalSet annotations (Zenodo 7061507, 412 MB) + VocalSet 1.1 audio
            (Zenodo 1442513, VocalSet11.zip, 2.1 GB); both CC BY 4.0.
            -> --root for `tune-bench vocalset|agent-vocalset|estimation --vocalset`
  medleydb  MedleyDB_Sample (Zenodo 1438309, 416 MB, CC BY-SA 4.0). The full MedleyDB needs an
            access request on Zenodo and is not fetched. -> --root for `tune-bench agent-medley`

Nothing here is committed to the repository. Read each licence before use.

Usage:  python fetch_zenodo.py --out D:/wave-datasets [vocalset] [medleydb]
"""
import argparse
import os
import sys
import tarfile
import urllib.request
import zipfile

SETS = {
    "vocalset": [
        ("https://zenodo.org/records/7061507/files/Annotated%20VocalSet.zip?download=1", "annotated.zip", 411531853),
        ("https://zenodo.org/records/1442513/files/VocalSet11.zip?download=1", "vocalset11.zip", 2077243579),
    ],
    "medleydb": [
        ("https://zenodo.org/records/1438309/files/MedleyDB_Sample.tar.gz?download=1", "MedleyDB_Sample.tar.gz", 415502011),
    ],
}


def download(url, dst, size):
    if os.path.exists(dst) and os.path.getsize(dst) == size:
        return
    tmp = dst + ".part"
    with urllib.request.urlopen(url, timeout=120) as r, open(tmp, "wb") as f:
        done = 0
        while True:
            b = r.read(1 << 20)
            if not b:
                break
            f.write(b)
            done += len(b)
            if done % (100 << 20) < (1 << 20):
                print(f"  {os.path.basename(dst)}: {done / 1e6:.0f} / {size / 1e6:.0f} MB", file=sys.stderr)
    if os.path.getsize(tmp) != size:
        raise RuntimeError(f"{dst}: got {os.path.getsize(tmp)} bytes, expected {size}")
    os.replace(tmp, dst)


def safe_members(names, root):
    root = os.path.realpath(root)
    for n in names:
        p = os.path.realpath(os.path.join(root, n))
        if not p.startswith(root + os.sep) and p != root:
            raise RuntimeError(f"refusing to extract outside the folder: {n}")


def unpack(path, into):
    if path.endswith(".zip"):
        with zipfile.ZipFile(path) as z:
            names = [n for n in z.namelist() if not n.startswith("__MACOSX")]
            safe_members(names, into)
            for n in names:
                z.extract(n, into)
    else:
        with tarfile.open(path) as t:
            members = [m for m in t.getmembers() if not os.path.basename(m.name).startswith("._") and (m.isfile() or m.isdir())]
            safe_members([m.name for m in members], into)
            t.extractall(into, members=members)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("sets", nargs="*", default=list(SETS))
    a = ap.parse_args()
    for name in a.sets:
        d = os.path.join(a.out, name)
        os.makedirs(d, exist_ok=True)
        for url, file, size in SETS[name]:
            dst = os.path.join(d, file)
            print(f"{name}: {file}", file=sys.stderr)
            download(url, dst, size)
            unpack(dst, d)
        print(f"{name}: ready in {d}", file=sys.stderr)


if __name__ == "__main__":
    main()
