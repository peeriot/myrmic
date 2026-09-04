use std::path::PathBuf;

use serde::Deserialize;
use sorg_client::types::FilestorePath;
use sorg_client::utils::validation_err;

use crate::{Error, Result};

/// Represents an entry -- in the document provided by the user -- describing a locally available file which
/// shall be brought into the filestore under a given path, if not alredy present
#[derive(Deserialize)]
struct FileManifest {
    local_path: String,
    fs_path: String,
}

/// Validated version of the ``FileManifest``. Guarantees (a) that the local path exists and points to a file and
/// (b) that the fs path is valid.
pub(super) struct File {
    pub(super) local_path: PathBuf,
    pub(super) fs_path: FilestorePath,
}

impl TryFrom<FileManifest> for File {
    type Error = Error;

    fn try_from(value: FileManifest) -> Result<Self, Self::Error> {
        let FileManifest {
            local_path,
            fs_path,
        } = value;
        let validated_local_path = std::fs::canonicalize(&local_path).map_err(|_| {
            validation_err!("the path '{local_path}' is not valid or does not exist.")
        })?;
        if !validated_local_path.is_file() {
            return Err(validation_err!("the path '{local_path}' does not point to a file").into());
        }
        let validated_fs_path = FilestorePath::new(fs_path)?;
        let validated_file = File {
            local_path: validated_local_path,
            fs_path: validated_fs_path,
        };
        Ok(validated_file)
    }
}

pub(super) fn read_file_manifest(file_manifest_path: PathBuf) -> Result<Vec<File>> {
    let bytes = std::fs::read(file_manifest_path)?;
    let file_manifests: Vec<FileManifest> = serde_yaml::from_slice(&bytes)?;
    file_manifests
        .into_iter()
        .map(std::convert::TryInto::try_into)
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::{Result, commands::files::manifest::File};

    use super::FileManifest;

    #[test]
    fn deser_working() {
        let input = r"
        - local_path: ./bla/file.txt
          fs_path: dir/name.txt
        - local_path: ./bla/file2.txt
          fs_path: dir/file.txt
        ";

        let mut manifests: Vec<FileManifest> = serde_yaml::from_str(input).unwrap();
        assert_eq!(2, manifests.len());

        let first = manifests.swap_remove(0);
        let conversion_result: Result<File> = first.try_into();
        assert!(conversion_result.is_err());
    }
}
