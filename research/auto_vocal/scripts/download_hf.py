"""Fetch MIR-1K (vocal channel + pitch labels) and GTZAN audio from Hugging Face mirrors
into D:\\mydaw-data\\raw. Both are research-only datasets: evaluation use only."""
from huggingface_hub import hf_hub_download, snapshot_download

RAW = "D:/mydaw-data/raw"
snapshot_download("AnhP/Mir-1k-use-DJCM-training", repo_type="dataset",
                  local_dir=f"{RAW}/mir1k",
                  allow_patterns=["MIR-1K/Wavfile/*", "MIR-1K/PitchLabel/*",
                                  "MIR-1K/vocal-nonvocalLabel/*"])
for song in ("Ani_1", "amy_1"):  # two full songs for the listening samples
    hf_hub_download("AnhP/Mir-1k-use-DJCM-training", f"MIR-1K/UndividedWavfile/{song}.wav", repo_type="dataset",
                    local_dir=f"{RAW}/mir1k")
print("mir1k done", flush=True)
snapshot_download("marsyas/gtzan", repo_type="dataset", local_dir=f"{RAW}/gtzan",
                  allow_patterns=["data/genres.tar.gz"])
print("gtzan done", flush=True)
