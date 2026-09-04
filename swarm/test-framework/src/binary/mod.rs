//! Resolving of locally built binaries (see also [`crate::resolve_binary!`]).

use std::path::PathBuf;

/// Resolve a binary with `name` inside the debug or release folder in `target_dir` while allowing
/// to completely override the resolving via a `{NAME}_BINARY` environment variable
/// (dashes in `name` are mapped to underscores, e.g. `test-cli` → `TEST_CLI_BINARY`).
///
/// Release builds take precedence over debug builds. Panics if the binary cannot be found.
pub fn resolve(target_dir: &str, name: &str) -> PathBuf {
    let env_var = format!("{}_BINARY", name.to_uppercase().replace('-', "_"));

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

    let target = PathBuf::from(target_dir);
    let release = target.join("release").join(name);
    let debug = target.join("debug").join(name);

    if release.exists() {
        release
    } else if debug.exists() {
        debug
    } else {
        panic!(
            "binary '{}' not found at {}/release/{name} or {}/debug/{name}; \
             neither is the ${env_var} environment variable set",
            name,
            target.display(),
            target.display(),
        )
    }
}
