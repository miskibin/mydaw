//! Portable project copies. Only project audio sources are collected; plugin references
//! and state stay in the project document and the binaries stay on disk.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

use crate::{engine::project_document, factory, Project};

pub(crate) fn write(mut project: Project, destination: &Path, origin: Option<&Path>) -> Result<(), String> {
    if !destination.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("zip")) {
        return Err("Choose a .zip file for the project archive".into());
    }
    let output = destination.canonicalize().ok();
    if output.is_some() && output == origin.and_then(|path| path.canonicalize().ok()) {
        return Err("The archive would replace the open project".into());
    }
    let folder = destination.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let project_name = origin.unwrap_or(destination).file_stem().and_then(|s| s.to_str()).unwrap_or("Song");
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated).compression_level(Some(1));
    let output_error = |error: &dyn std::fmt::Display| format!("{}: {error}", destination.display());
    let mut temp = tempfile::NamedTempFile::new_in(folder).map_err(|e| output_error(&e))?;
    let mut zip = ZipWriter::new(temp.as_file_mut());
    let mut names = HashMap::<PathBuf, String>::new();

    for source in &mut project.sources {
        let original = &source.path;
        let source_error = |error: &dyn std::fmt::Display| format!("{} ({original}): {error}", source.name);
        let embedded = factory::embedded(original);
        let key = if embedded.is_some() {
            PathBuf::from(original)
        } else {
            let path = if original.starts_with(factory::SCHEME) {
                factory::resolve(original).ok_or_else(|| source_error(&"invalid library path"))?
            } else {
                PathBuf::from(original)
            };
            // The engine resolves project-relative paths on open. Canonicalizing here also
            // collects one copy when sources reach the same file via different paths.
            path.canonicalize().map_err(|e| source_error(&e))?
        };
        if let Some(name) = names.get(&key) {
            source.path.clone_from(name);
            continue;
        }

        let file_name = Path::new(original).file_name().and_then(|s| s.to_str()).unwrap_or("sample");
        let name = format!("Audio/{}-{}", names.len() + 1, portable_filename(file_name));
        if let Some(sample) = embedded {
            zip.start_file(&name, options).map_err(|e| output_error(&e))?;
            zip.write_all(sample.bytes).map_err(|e| source_error(&e))?;
        } else {
            if output.as_ref() == Some(&key) {
                return Err(source_error(&"the archive would replace this source"));
            }
            let metadata = key.metadata().map_err(|e| source_error(&e))?;
            if !metadata.is_file() {
                return Err(source_error(&"expected an audio file"));
            }
            let mut file = File::open(&key).map_err(|e| source_error(&e))?;
            // Let DEFLATE grow slightly on incompressible input near the 4 GiB limit.
            zip.start_file(&name, options.large_file(metadata.len() >= 4_000_000_000)).map_err(|e| output_error(&e))?;
            io::copy(&mut file, &mut zip).map_err(|e| source_error(&e))?;
        }
        source.path.clone_from(&name);
        names.insert(key, name);
    }

    zip.start_file(format!("{}.wave", portable_name(project_name)), options).map_err(|e| output_error(&e))?;
    let document = serde_json::to_vec_pretty(&project_document(&project)).map_err(|e| output_error(&e))?;
    zip.write_all(&document).map_err(|e| output_error(&e))?;
    zip.finish().map_err(|e| output_error(&e))?;
    temp.persist(destination).map_err(|e| output_error(&e.error))?;
    Ok(())
}

fn portable_filename(name: &str) -> String {
    let path = Path::new(name);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("sample");
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        format!("{}.{}", portable_name(stem), portable_name(ext))
    } else {
        portable_name(stem)
    }
}

fn portable_name(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c }).take(60).collect();
    let cleaned = cleaned.trim().trim_end_matches(['.', ' ']);
    let device = cleaned.split('.').next().unwrap_or_default().trim_end().to_ascii_uppercase();
    let numbered_device = device.strip_prefix("COM").or_else(|| device.strip_prefix("LPT"));
    if cleaned.is_empty()
        || matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || numbered_device.is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"))
    {
        format!("_{cleaned}")
    } else {
        cleaned.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{catalog::new_fx, Engine, FxKind, NoPlugins, Op, PluginRef};
    use std::fs;
    use std::sync::Arc;

    #[test]
    fn archive_reopens_without_original_media_and_preserves_the_open_project() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("original");
        let destination = root.path().join("share.zip");
        let unpacked = root.path().join("unpacked");
        let project_file = original.join("Song.wave");
        let first = original.join("a/same.flac");
        let second = original.join("b/same.flac");
        for path in [&first, &second] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
        }
        fs::write(&first, factory::files()[0].bytes).unwrap();
        fs::write(&second, factory::files()[1].bytes).unwrap();
        let plugin = original.join("example.vst3");
        fs::write(&plugin, b"plugin binary").unwrap();

        let mut project = Project::empty();
        let mut fx = new_fx(project.alloc_id(), FxKind::Vst3, project.bpm);
        fx.plugin = Some(PluginRef {
            format: "vst3".into(),
            path: plugin.display().to_string(),
            cid: String::new(),
            name: "Example".into(),
            vendor: String::new(),
            state: Some("c3RhdGU=".into()),
        });
        project.inserts[0].fx.push(fx);
        let engine = Engine::new(project, Arc::new(NoPlugins));
        let (imported, _) = engine.import_audio(&[first.clone(), second, first], Some((0, 0.0))).unwrap();
        engine.import_audio(&[PathBuf::from(factory::lib_path(factory::files()[2].file))], None).unwrap();
        engine.save(&project_file).unwrap();
        assert!(engine.open(&project_file).unwrap().is_empty());
        engine.apply(&[Op::SetBpm { bpm: 95.0 }], false, None).unwrap();
        let before = engine.state();
        engine.export_project_zip(&destination, Some(before.revision)).unwrap();
        let after = engine.state();
        assert!(after.dirty);
        assert_eq!(after.path, before.path);
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.project, before.project);

        let mut archive = zip::ZipArchive::new(File::open(destination).unwrap()).unwrap();
        assert_eq!(archive.len(), before.project.sources.len(), "the shared sample is copied once and no binary is included");
        archive.extract(&unpacked).unwrap();
        fs::rename(&original, root.path().join("offline")).unwrap();
        let reopened = Engine::new(Project::empty(), Arc::new(NoPlugins));
        assert!(reopened.open(&unpacked.join("Song.wave")).unwrap().is_empty());
        let mut recovered = reopened.project();
        let path_for = |id| &recovered.sources.iter().find(|s| s.id == id).unwrap().path;
        assert_eq!(path_for(imported[0]), path_for(imported[2]));
        assert_ne!(path_for(imported[0]), path_for(imported[1]));
        assert!(recovered.sources.iter().all(|s| Path::new(&s.path).starts_with(unpacked.join("Audio"))));
        for (new, old) in recovered.sources.iter_mut().zip(&before.project.sources) {
            new.path.clone_from(&old.path);
        }
        assert_eq!(recovered, before.project);
    }

    #[test]
    fn a_missing_source_preserves_the_previous_archive_and_leaves_no_partial_file() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("share.zip");
        let source = root.path().join("missing.flac");
        fs::write(&source, factory::files()[0].bytes).unwrap();
        let engine = Engine::new(Project::sketch(), Arc::new(NoPlugins));
        engine.import_audio(std::slice::from_ref(&source), None).unwrap();
        engine.export_project_zip(&destination, None).unwrap();
        let previous = fs::read(&destination).unwrap();
        fs::remove_file(&source).unwrap();

        let error = engine.export_project_zip(&destination, None).unwrap_err();
        assert!(error.to_string().contains("missing.flac"));
        assert_eq!(fs::read(&destination).unwrap(), previous);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        let revision = engine.revision();
        assert!(engine.export_project_zip(&destination, Some(revision + 1)).unwrap_err().to_string().starts_with("REVISION_CONFLICT"));
    }
}
