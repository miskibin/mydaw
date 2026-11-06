//! The sample library: the factory kits (downloaded on demand, every file
//! pinned by SHA-256 in `assets-manifest.json`) and folders the user adds.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MANIFEST: &str = include_str!("../../assets-manifest.json");
const AUDIO_EXT: &[&str] = &["wav", "flac", "ogg", "oga", "mp3", "aif", "aiff"];

#[derive(Deserialize)]
struct Manifest {
    instruments: Vec<Kit>,
}

#[derive(Deserialize, Clone)]
struct Kit {
    id: String,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    attribution: String,
    samples: Vec<Sample>,
}

#[derive(Deserialize, Clone)]
struct Sample {
    file: String,
    url: String,
    sha256: String,
    bytes: u64,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KitInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub bytes: u64,
    pub count: usize,
    pub installed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    pub name: String,
    pub path: String,
    pub group: String,
}

fn manifest() -> Manifest {
    serde_json::from_str(MANIFEST).expect("embedded manifest is valid")
}

pub fn library_dir() -> PathBuf {
    wave_engine::engine::scratch_dir().parent().unwrap().join("Library")
}

fn kit_dir(id: &str) -> PathBuf {
    library_dir().join(id)
}

fn installed(k: &Kit) -> bool {
    let d = kit_dir(&k.id);
    k.samples.iter().all(|s| d.join(&s.file).is_file())
}

pub fn kits() -> Vec<KitInfo> {
    manifest()
        .instruments
        .into_iter()
        .map(|k| KitInfo { installed: installed(&k), bytes: k.samples.iter().map(|s| s.bytes).sum(), count: k.samples.len(), id: k.id, name: k.name, description: k.description })
        .collect()
}

/// Downloads every missing file of a kit, verifying checksums. Blocking.
pub fn install(id: &str, progress: impl Fn(usize, usize)) -> Result<(), String> {
    let kit = manifest().instruments.into_iter().find(|k| k.id == id).ok_or(format!("no kit {id}"))?;
    let dir = kit_dir(id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let total = kit.samples.len();
    for (i, s) in kit.samples.iter().enumerate() {
        progress(i, total);
        let dst = dir.join(&s.file);
        if dst.is_file() {
            continue;
        }
        let mut resp = ureq::get(&s.url).call().map_err(|e| format!("{}: {e}", s.file))?;
        let mut bytes = Vec::with_capacity(s.bytes as usize);
        resp.body_mut().as_reader().read_to_end(&mut bytes).map_err(|e| format!("{}: {e}", s.file))?;
        let got = hex(&Sha256::digest(&bytes));
        if !got.eq_ignore_ascii_case(&s.sha256) {
            return Err(format!("{}: checksum mismatch — the download was altered or damaged; nothing was saved", s.file));
        }
        let tmp = dst.with_extension("part");
        std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &dst).map_err(|e| e.to_string())?;
    }
    if !kit.attribution.is_empty() {
        let _ = std::fs::write(dir.join("ATTRIBUTION.txt"), &kit.attribution);
    }
    progress(total, total);
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn kit_files(id: &str) -> Vec<FileInfo> {
    let Some(kit) = manifest().instruments.into_iter().find(|k| k.id == id) else { return vec![] };
    let dir = kit_dir(id);
    kit.samples
        .iter()
        .filter(|s| dir.join(&s.file).is_file())
        .map(|s| {
            let note = s.note.clone().unwrap_or_default();
            let group = note.split([',', ' ']).next().filter(|g| !g.is_empty()).unwrap_or(&kit.name).to_string();
            FileInfo { name: if note.is_empty() { s.file.clone() } else { note }, path: dir.join(&s.file).display().to_string(), group }
        })
        .collect()
}

/// Audio files under `root` (recursive, at most 5000), grouped by sub-folder.
pub fn folder_files(root: &Path) -> Vec<FileInfo> {
    let mut out = vec![];
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()).is_some_and(|x| AUDIO_EXT.contains(&x.to_ascii_lowercase().as_str())) {
                let group = p.parent().and_then(|q| q.strip_prefix(root).ok()).map(|q| q.display().to_string()).filter(|g| !g.is_empty()).unwrap_or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                out.push(FileInfo { name: p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), path: p.display().to_string(), group });
                if out.len() >= 5000 {
                    return out;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn manifest_parses_and_has_kits() {
        let k = super::kits();
        assert!(k.iter().any(|k| k.id == "808-kit"));
        assert!(k.iter().all(|k| k.count > 0 && k.bytes > 0));
    }
}
