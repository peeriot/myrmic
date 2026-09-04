use anyhow::Context;
use std::path::{Path, PathBuf};

pub fn write(path: impl AsRef<Path>, entries: &[Entry]) -> anyhow::Result<()> {
    let path = path.as_ref();
    let file = std::fs::File::create(path)
        .with_context(|| format!("unable to create nest file: {}", path.display()))?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut tar = tar::Builder::new(encoder);

    for entry in entries {
        match entry {
            Entry::Path(name, artifact) => {
                let file_name = if let Some(name) = name {
                    Path::new(name)
                } else {
                    let path = artifact.file_name().with_context(|| {
                        format!("artifact has no file name: {}", artifact.display())
                    })?;
                    Path::new(path)
                };
                tar.append_path_with_name(artifact, file_name)
                    .with_context(|| format!("unable to add to nest: {}", artifact.display()))?;
            }
            Entry::Virtual(name, content) => {
                let bytes = content.as_bytes();
                let mtime = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                header.set_mtime(mtime);
                header.set_cksum();
                tar.append_data(&mut header, name, bytes)
                    .with_context(|| format!("unable to add to nest: {}", name))?;
            }
        }
    }
    let encoder = tar.into_inner().context("unable to finalize nest file")?;
    encoder.finish().context("unable to finalize gzip stream")?;

    Ok(())
}

pub enum Entry {
    Path(Option<String>, PathBuf),
    Virtual(String, String),
}

impl Entry {
    pub fn from_path(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self::Path(Some(name.into()), path.into())
    }

    pub fn virt(name: impl Into<String>, content: impl Into<String>) -> Self {
        Self::Virtual(name.into(), content.into())
    }
}
