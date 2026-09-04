//! Cell sources and deployed-cell handles for the myrmic shim.

use std::future::Future;
use tempfile::TempDir;

use crate::myrmic::MyrmicBackend;

/// Where a cell's sources live: an existing path or a temporary directory
/// (e.g. created by `myrmic new`) that is deleted on drop.
pub enum CellSpec {
    /// cell sources at a fixed path
    Path(std::path::PathBuf),
    /// cell sources in a temporary directory, deleted when the spec is dropped
    Temporary(TempDir),
}

impl CellSpec {
    /// the path to the cell sources
    pub fn as_path(&self) -> &std::path::Path {
        match self {
            CellSpec::Path(path_buf) => path_buf,
            CellSpec::Temporary(temp_dir) => temp_dir.path(),
        }
    }
}

impl<T> From<T> for CellSpec
where
    T: Into<std::path::PathBuf>,
{
    fn from(value: T) -> Self {
        Self::Path(value.into())
    }
}

/// a deployed cell is the outcome of `myrmic deploy`
///
/// Dropping a `DeployedCell` deletes it best-effort (panic-safe cleanup). Call
/// [`DeployedCell::delete`] instead when the test asserts on the post-delete state.
pub struct DeployedCell<B>
where
    B: MyrmicBackend,
{
    sri: String,
    backend: B,
    armed: bool,
}

impl<B> DeployedCell<B>
where
    B: MyrmicBackend + Clone,
{
    /// track an already-deployed cell at `sri`; the drop guard is armed
    pub fn new(backend: B, sri: impl Into<String>) -> Self {
        Self {
            backend,
            sri: sri.into(),
            armed: true,
        }
    }

    /// the SRI the cell was deployed under
    pub fn sri(&self) -> &str {
        &self.sri
    }

    /// run: myrmic send `sri` `command`; returns the command response, if any
    pub fn send(&self, command: impl Into<String>) -> impl Future<Output = Option<String>> {
        let backend = self.backend.clone();
        let sri = self.sri.clone();
        let command = command.into();
        async move { backend.send(&sri, &command).await }
    }

    /// run: myrmic delete; returns once the SRI is no longer in `myrmic status`
    pub async fn delete(mut self) {
        self.armed = false;
        self.backend.delete_cell(&self.sri).await;
        let gone = crate::wait_until(
            crate::wait::DEFAULT_TIMEOUT,
            crate::wait::DEFAULT_POLL_INTERVAL,
            || async {
                !self
                    .backend
                    .status()
                    .await
                    .iter()
                    .any(|line| line.contains(&self.sri))
            },
        )
        .await;
        assert!(gone, "SRI `{}` still in status 10s after delete", self.sri);
    }
}

impl<B> Drop for DeployedCell<B>
where
    B: MyrmicBackend,
{
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Err(e) = self.backend.delete_cell_blocking(&self.sri) {
            eprintln!(
                "DeployedCell drop-guard: failed to delete cell `{}`: {e}",
                self.sri
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::DeployedCell;
    use crate::myrmic::MyrmicBackend;
    use crate::myrmic::cell::CellSpec;

    #[derive(Clone, Default)]
    struct FakeBackend {
        blocking_deletes: Arc<AtomicU32>,
        async_deletes: Arc<AtomicU32>,
    }

    impl MyrmicBackend for FakeBackend {
        async fn start_runtime(&self, _: &str, _: &[&str]) {}
        async fn delete_runtime(&self, _: &str) {}
        async fn list_runtimes(&self) -> Vec<String> {
            vec![]
        }
        async fn new_cell(&self, _: &std::path::Path, _: &str, _: Option<&str>) {}
        async fn status(&self) -> Vec<String> {
            vec![]
        }
        async fn send(&self, _: &str, _: &str) -> Option<String> {
            None
        }
        async fn deploy(&self, _: CellSpec, _: &str, _: &[&str]) {}
        async fn deploy_app(&self, _: &std::path::Path) {}
        async fn delete_cell(&self, _: &str) {
            self.async_deletes.fetch_add(1, Ordering::SeqCst);
        }
        fn delete_runtime_blocking(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn delete_cell_blocking(&self, _: &str) -> Result<(), String> {
            self.blocking_deletes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn drop_guard_deletes_cell() {
        let backend = FakeBackend::default();
        drop(DeployedCell::new(backend.clone(), "sri"));
        assert_eq!(1, backend.blocking_deletes.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn explicit_delete_disarms_guard() {
        let backend = FakeBackend::default();
        DeployedCell::new(backend.clone(), "sri").delete().await;
        assert_eq!(1, backend.async_deletes.load(Ordering::SeqCst));
        assert_eq!(0, backend.blocking_deletes.load(Ordering::SeqCst));
    }
}
