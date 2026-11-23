//! Finding the locally built binaries (see also [`crate::resolve_binary!`]).

use std::path::{Path, PathBuf};

/// The target directory of the workspace this crate is built in, wherever it is used from.
///
/// This is only right while this crate is a path dependency inside that workspace. As a git
/// dependency, its sources are in cargo's checkout cache, where nothing is built, so only the
/// environment variables of [`resolve`] can find a binary.
const TARGET_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target");

/// Find the binary `name` built for `triple` (`None`: the test's machine) in the workspace's target
/// directory, unless an environment variable names the binary.
///
/// The variable is `{NAME}_BINARY` for the test's machine and `{NAME}_{TRIPLE}_BINARY` for a
/// triple, upper case, with dashes replaced by underscores: `zenoh-router` → `ZENOH_ROUTER_BINARY`,
/// `myrmic` for `aarch64-unknown-linux-gnu` → `MYRMIC_AARCH64_UNKNOWN_LINUX_GNU_BINARY`.
///
/// Without the variable, the binary is looked for in the target directory, or for a triple in its
/// `<triple>` directory (where cargo puts a `--target` build). With `profile`, it is looked for in
/// that profile's directory. Without one, the most recently modified binary of all profiles'
/// directories is taken, so an old build of one profile doesn't hide a new build of another. Panics
/// if there is none.
pub fn resolve(name: &str, triple: Option<&str>, profile: Option<&str>) -> PathBuf {
    resolve_in(TARGET_DIR, name, triple, profile)
}

/// [`resolve`] in `target_dir`
fn resolve_in(
    target_dir: &str,
    name: &str,
    triple: Option<&str>,
    profile: Option<&str>,
) -> PathBuf {
    let env_var = match triple {
        None => format!("{name}_BINARY"),
        Some(triple) => format!("{name}_{triple}_BINARY"),
    }
    .to_uppercase()
    .replace('-', "_");

    if let Ok(path) = std::env::var(&env_var) {
        let p = PathBuf::from(&path);
        assert!(
            p.exists(),
            "binary '{}' set via ${env_var} not found at {}",
            name,
            p.display()
        );
        return p;
    }

    let mut target = PathBuf::from(target_dir);
    if let Some(triple) = triple {
        target.push(triple);
    }
    match profile {
        Some(profile) => {
            let binary = target.join(profile).join(name);
            assert!(
                binary.exists(),
                "binary '{name}' not found at {}; neither is the ${env_var} environment variable \
                 set",
                binary.display()
            );
            binary
        }
        None => newest_build(&target, name).unwrap_or_else(|| {
            panic!(
                "binary '{name}' not found in any profile directory of {}; neither is the \
                 ${env_var} environment variable set",
                target.display()
            )
        }),
    }
}

/// the most recently modified `<profile>/<name>` of every directory `<profile>` in `target`, if
/// any
fn newest_build(target: &Path, name: &str) -> Option<PathBuf> {
    let entries = match std::fs::read_dir(target) {
        Ok(entries) => entries,
        // nothing was built for this target
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => panic!("failed to read {}: {e}", target.display()),
    };
    entries
        .map(|entry| entry.unwrap_or_else(|e| panic!("failed to read {}: {e}", target.display())))
        // also files like `CACHEDIR.TAG`
        .filter(|entry| {
            entry
                .file_type()
                .unwrap_or_else(|e| panic!("failed to stat {}: {e}", entry.path().display()))
                .is_dir()
        })
        .filter_map(|profile| {
            let binary = profile.path().join(name);
            match std::fs::metadata(&binary) {
                Ok(metadata) => {
                    let modified = metadata.modified().unwrap_or_else(|e| {
                        panic!(
                            "the platform reports no mtime for {}: {e}",
                            binary.display()
                        )
                    });
                    Some((modified, binary))
                }
                // a profile it wasn't built in, or no profile at all (a triple's directory)
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => panic!("failed to stat {}: {e}", binary.display()),
            }
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, binary)| binary)
}

#[cfg(test)]
mod tests {
    use std::{
        fs::File,
        time::{Duration, SystemTime},
    };

    use super::resolve_in;

    /// a binary `name` under `target/<profile>`, last modified `age` ago
    fn build(target: &std::path::Path, profile: &str, name: &str, age: Duration) {
        let dir = target.join(profile);
        std::fs::create_dir_all(&dir).expect("failed to create the profile directory");
        let binary = File::create(dir.join(name)).expect("failed to create the binary");
        binary
            .set_modified(SystemTime::now() - age)
            .expect("failed to set the binary's mtime");
    }

    #[test]
    fn the_most_recent_build_of_any_profile_wins() {
        let target = tempfile::tempdir().expect("failed to create a target directory");
        let name = "resolve-test-newest";
        File::create(target.path().join("CACHEDIR.TAG")).expect("failed to create a tag file");
        build(target.path(), "release", name, Duration::from_secs(3600));
        build(target.path(), "custom", name, Duration::from_secs(60));
        build(target.path(), "debug", name, Duration::from_secs(600));

        let target_dir = target.path().to_str().expect("a temp path is UTF-8");
        assert_eq!(
            resolve_in(target_dir, name, None, None),
            target.path().join("custom").join(name)
        );
    }

    #[test]
    fn a_pinned_profile_wins_over_a_newer_build() {
        let target = tempfile::tempdir().expect("failed to create a target directory");
        let name = "resolve-test-pinned";
        build(target.path(), "release", name, Duration::from_secs(3600));
        build(target.path(), "debug", name, Duration::ZERO);

        let target_dir = target.path().to_str().expect("a temp path is UTF-8");
        assert_eq!(
            resolve_in(target_dir, name, None, Some("release")),
            target.path().join("release").join(name)
        );
    }

    #[test]
    fn a_triple_looks_under_its_own_directory() {
        let target = tempfile::tempdir().expect("failed to create a target directory");
        let name = "resolve-test-triple";
        let triple = "aarch64-unknown-linux-gnu";
        build(target.path(), "release", name, Duration::ZERO);
        build(
            &target.path().join(triple),
            "dist",
            name,
            Duration::from_secs(600),
        );

        let target_dir = target.path().to_str().expect("a temp path is UTF-8");
        assert_eq!(
            resolve_in(target_dir, name, Some(triple), None),
            target.path().join(triple).join("dist").join(name)
        );
    }
}
