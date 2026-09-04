//! Shared functionality concerning the file system, file paths, etc.

use std::{
    env,
    fmt::Display,
    fs::{self, File},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{Result, bail, bail_validation};

/// Used as the payload of the error reply of the get query to the file store to indicate that the requested file
/// was missing
#[derive(Serialize, Deserialize)]
pub struct MissingFileRecord {}

/// Represents a writable directory of the file store. The type guarantees that contained directory
/// (a) exists and (b) can be modified (creating/deleting files) with the rights that the filestore is
/// executed with
#[derive(Debug, Clone)]
pub struct WritableDirectory {
    path: PathBuf,
}

impl AsRef<Path> for WritableDirectory {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl WritableDirectory {
    pub fn new(dir: &str) -> Result<Self> {
        let path = PathBuf::from(dir);
        if !path.exists() {
            bail_validation!("The directory at the path '{dir}' does not exist.");
        }
        if !path.is_dir() {
            bail_validation!("The path '{dir}' does not point to a directory.");
        }
        if !is_dir_writable(&path)? {
            bail_validation!("The directory at the path '{dir}' is not writable.");
        }
        let Ok(cur_dir) = env::current_dir() else {
            bail!("failed to get current dir");
        };
        let path = cur_dir.join(path);
        if is_symlink(&path)? {
            bail!("the resolved path points to a symlink")
        }

        let validated_dir = Self { path };
        Ok(validated_dir)
    }

    #[must_use]
    pub fn resolve(&self, file_path: &FilestorePath) -> PathBuf {
        self.as_ref().join(file_path.as_ref())
    }
}

fn is_symlink(path: &Path) -> Result<bool> {
    Ok(std::fs::symlink_metadata(path)?.file_type().is_symlink())
}

fn is_dir_writable(path: &Path) -> Result<bool> {
    let temp_file = path.join(".writable_check.tmp");
    match File::create(&temp_file) {
        Ok(_) => {
            // we can write there
            fs::remove_file(temp_file)?;
            Ok(true)
        }
        Err(_) => Ok(false),
    }
}

/// Represents a path within the file store as specified by the user (relatively to the root dir). The type
/// guarantees following invariants:
/// - does not contain :, *, ?, <, >, |, ", and, in particular .. (so that we cannot point outside the root dir)
/// - does not start or end with a /
/// - is not empty
/// - does not contain spaces/new lines/tabs (after removing trailers - those are okay and removed during construction)
#[derive(Debug, Serialize, PartialEq, Eq, Clone)]
#[serde(transparent)]
pub struct FilestorePath(String);

impl Display for FilestorePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{path}", path = self.0)
    }
}

impl AsRef<str> for FilestorePath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for FilestorePath {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s: String = Deserialize::deserialize(deserializer)?;
        FilestorePath::new(s).map_err(serde::de::Error::custom)
    }
}

impl FilestorePath {
    pub fn new(rel_path: impl Into<String>) -> Result<Self> {
        let input = rel_path.into();
        let trimmed = input.trim().to_owned();
        if trimmed.is_empty() {
            bail_validation!("A relative path must not be empty");
        }
        if let Some(forbidden) = find_forbidden_char(&trimmed) {
            bail_validation!(
                "The relative path '{trimmed}' contains the forbidden character ' {forbidden} '"
            );
        }
        if let Some(forbidden) = find_inner_spaces(&trimmed) {
            bail_validation!(
                "The relative path '{trimmed}' contains an inner whitespace character: ' {forbidden} '"
            );
        }
        if trimmed.ends_with('/') {
            bail_validation!(
                "The relative path '{trimmed}' ends with a slash, which is not allowed."
            )
        }
        if trimmed.contains("..") {
            bail_validation!("The relative path '{trimmed}' contains '..', which is not allowed.")
        }
        Ok(Self(trimmed))
    }
}

fn find_inner_spaces(string: &str) -> Option<char> {
    let forbidden = [' ', '\n', '\t', '\r'];
    string.chars().find(|c| forbidden.contains(c))
}

fn find_forbidden_char(string: &str) -> Option<char> {
    let forbidden = [':', '*', '?', '<', '>', '|', '"'];
    string.chars().find(|c| forbidden.contains(c))
}

#[cfg(test)]
mod tests {
    use std::{env, fs, path::PathBuf};

    use claims::{assert_err, assert_ok};

    use crate::{
        SorgPayload,
        fs::{FilestorePath, WritableDirectory},
    };

    #[test]
    fn ser_deser_fs_path_yaml() {
        let path = "./wasm/building/sensor.wasm";
        let rel_path = FilestorePath::new(path).unwrap();
        let serialized = serde_yaml::to_string(&rel_path).unwrap();
        let deserialized: FilestorePath = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(rel_path.as_ref(), deserialized.as_ref());
    }

    #[test]
    fn ser_deser_fs_path_bincode() {
        let path = "./wasm/building/sensor.wasm";
        let rel_path = FilestorePath::new(path).unwrap();
        let serialized = rel_path.clone().to_payload().unwrap();
        let deserialized = FilestorePath::from_payload(&serialized, "test").unwrap();
        assert_eq!(rel_path.as_ref(), deserialized.as_ref());
    }

    #[test]
    fn directory_does_not_exist() {
        assert_err!(WritableDirectory::new("./non_existing"));
    }

    #[test]
    fn no_write_permissions() {
        assert_err!(WritableDirectory::new("/etc"));
    }

    #[test]
    fn correct_writable_dir() {
        let dir = assert_ok!(WritableDirectory::new("."));
        let expected = env::current_dir().unwrap();
        assert_eq!(expected, dir.as_ref());
    }

    #[test]
    fn empty_rel_path() {
        assert_err!(FilestorePath::new(""));
    }

    #[test]
    fn forbidden_chars_in_rel_path() {
        assert_err!(FilestorePath::new("my:file"));
        assert_err!(FilestorePath::new("myfile*"));
        assert_err!(FilestorePath::new("my<file"));
        assert_err!(FilestorePath::new("my>file"));
        assert_err!(FilestorePath::new("my|file"));
        assert_err!(FilestorePath::new("my\"file"));
        assert_err!(FilestorePath::new("myfile/../../../super_secret_file"));
    }

    #[test]
    fn intermittent_spaces_in_rel_path() {
        assert_err!(FilestorePath::new("my file"));
        assert_err!(FilestorePath::new("my\tfile"));
        assert_err!(FilestorePath::new("my\nfile"));
    }

    #[test]
    fn trailing_spaces_in_rel_path() {
        let path = assert_ok!(FilestorePath::new("myfile.txt "));
        assert_eq!("myfile.txt", path.as_ref());
        let path = assert_ok!(FilestorePath::new("myfile\t"));
        assert_eq!("myfile", path.as_ref());
        let path = assert_ok!(FilestorePath::new("myfile\n"));
        assert_eq!("myfile", path.as_ref());
    }

    #[test]
    fn starting_or_ending_slash_rel_path() {
        assert_err!(FilestorePath::new("my_file/"));
    }

    #[test]
    fn usual_case() {
        let path = assert_ok!(FilestorePath::new("myfile"));
        assert_eq!("myfile", path.as_ref());
    }

    #[test]
    fn usual_case_multi_level() {
        let path = assert_ok!(FilestorePath::new("mydir/myfile"));
        assert_eq!("mydir/myfile", path.as_ref());
    }

    #[test]
    fn general_case() {
        test_template_file_path(
            "./file-store1",
            "documents/report.txt",
            "./file-store1/documents/report.txt",
        );
    }

    #[test]
    fn root_dir_ends_with_slash() {
        test_template_file_path(
            "./file-store2/",
            "documents/report.txt",
            "./file-store2/documents/report.txt",
        );
    }

    #[test]
    fn multi_level_file_path() {
        test_template_file_path(
            "./file-store3/",
            "nested/dir/documents/report.txt",
            "./file-store3/nested/dir/documents/report.txt",
        );
    }

    #[test]
    fn just_a_file() {
        test_template_file_path("./file-store4/", "report.txt", "./file-store4/report.txt");
    }

    fn test_template_file_path(root_dir: &str, rel_path: &str, expected: &str) {
        assert!(
            !PathBuf::from(root_dir).exists(),
            "the directory {root_dir} exists on the system and must not be used for tests"
        );
        fs::create_dir(root_dir).unwrap();
        let _guard = DropGuard {
            created_dir: root_dir.to_owned(),
        };
        let writable_dir = WritableDirectory::new(root_dir).unwrap();
        let relative_path = FilestorePath::new(rel_path).unwrap();
        let full_file_path = writable_dir.resolve(&relative_path);
        let expected_path = env::current_dir().unwrap().join(expected);
        assert_eq!(expected_path, full_file_path);
    }

    struct DropGuard {
        created_dir: String,
    }

    impl Drop for DropGuard {
        fn drop(&mut self) {
            fs::remove_dir(&self.created_dir).unwrap();
        }
    }
}
