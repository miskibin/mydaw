"""GiantSteps Tempo audio (664 Beatport 2-min previews) from the official JKU backup host
named in the dataset's own audio_dl.sh. Research-only audio: evaluation use only."""
import concurrent.futures as cf
import pathlib
import urllib.request

ANN = pathlib.Path(r"D:\mydaw-data\gs_tempo_ann\giantsteps-tempo-dataset-0b7d47ba8cae59d3535a02e3db69e2cf6d0af5bb")
OUT = pathlib.Path(r"D:\mydaw-data\gs_tempo_audio")
OUT.mkdir(parents=True, exist_ok=True)
BASE = "http://www.cp.jku.at/datasets/giantsteps/backup/"
ids = sorted(p.name.split(".")[0] for p in (ANN / "annotations" / "tempo").glob("*.bpm"))


def get(i):
    dst = OUT / f"{i}.LOFI.mp3"
    if dst.exists() and dst.stat().st_size > 10000:
        return 0
    try:
        urllib.request.urlretrieve(BASE + dst.name, dst)
        return 0
    except Exception as e:  # noqa: BLE001
        print("FAIL", i, e, flush=True)
        return 1


with cf.ThreadPoolExecutor(4) as ex:
    fails = sum(ex.map(get, ids))
print("done", len(ids), "fails", fails, flush=True)
