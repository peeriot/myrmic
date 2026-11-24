//! Cells for the shim: where a cell's sources are, and handles of deployed cells and apps.

use std::path::{Path, PathBuf};

use crate::myrmic::{DEPLOYMENT_GONE, Error, Myrmic, MyrmicBackend, already_gone};

/// Where a cell's sources are on the target (the machine the CLI runs on): an existing path, or a
/// temporary directory (e.g. created by `myrmic new`) that is removed on drop.
pub enum CellSpec<B>
where
    B: MyrmicBackend,
{
    /// cell sources at a fixed path
    Path(PathBuf),
    /// cell sources in a temporary directory, removed when the spec is dropped
    Temporary(TargetTempDir<B>),
}

impl<B> CellSpec<B>
where
    B: MyrmicBackend,
{
    /// the path to the cell sources
    pub fn as_path(&self) -> &Path {
        match self {
            CellSpec::Path(path_buf) => path_buf,
            CellSpec::Temporary(temp_dir) => &temp_dir.path,
        }
    }
}

impl<B, T> From<T> for CellSpec<B>
where
    B: MyrmicBackend,
    T: Into<PathBuf>,
{
    fn from(value: T) -> Self {
        Self::Path(value.into())
    }
}

/// A temporary directory on the target, created with `mktemp -d` and removed with `rm -rf` when
/// dropped. A temporary directory on the test's machine wouldn't do: with the SSH and docker
/// backends, the CLI writes into the target's file system.
pub struct TargetTempDir<B>
where
    B: MyrmicBackend,
{
    myrmic: Myrmic<B>,
    path: PathBuf,
}

impl<B> TargetTempDir<B>
where
    B: MyrmicBackend,
{
    /// Create a directory named `<prefix>.XXXXXX` in the target's temporary directory. A target
    /// without a working `mktemp` means the test infrastructure is broken, so a failure panics.
    pub(super) async fn create(myrmic: &Myrmic<B>, prefix: &str) -> Self {
        let template = format!("{prefix}.XXXXXX");
        let output = myrmic.run_program("mktemp", &["-d", "-t", &template]).await;
        assert!(
            output.success,
            "failed to create a temporary directory on the target: {}",
            output.stderr
        );
        Self {
            myrmic: myrmic.clone(),
            path: PathBuf::from(output.stdout.trim()),
        }
    }

    /// Write `content` to the file `name` in this directory and return its path. A failure is
    /// broken test infrastructure and panics.
    pub(super) async fn write(&self, name: &str, content: &str) -> PathBuf {
        let path = self.path.join(name);
        let path_str = path
            .to_str()
            .expect("the directory came from `mktemp` output, which is UTF-8");
        let output = self
            .myrmic
            .run_program(
                "sh",
                &["-c", "printf '%s' \"$1\" > \"$2\"", "sh", content, path_str],
            )
            .await;
        assert!(
            output.success,
            "failed to write `{path_str}` on the target: {}",
            output.stderr
        );
        path
    }
}

impl<B> Drop for TargetTempDir<B>
where
    B: MyrmicBackend,
{
    fn drop(&mut self) {
        let path = self
            .path
            .to_str()
            .expect("the path came from `mktemp` output, which is UTF-8");
        match self.myrmic.run_program_blocking("rm", &["-rf", path]) {
            Ok(output) if output.success => {}
            Ok(output) => eprintln!(
                "TargetTempDir drop-guard: failed to remove `{path}`: {}",
                output.stderr
            ),
            Err(err) => eprintln!("TargetTempDir drop-guard: failed to run rm -rf `{path}`: {err}"),
        }
    }
}

/// A deployed cell: deployed with `myrmic deploy --name <srn> <cell>` (see [`Myrmic::cell`]), or
/// through a swarm's sorg client (see [`crate::swarm::Swarm::deploy`]).
///
/// Dropping a `DeployedCell` deletes the cell, best effort, so cleanup also happens when the test
/// panics. The drop also deletes after an explicit [`DeployedCell::delete`]; a cell that is
/// already gone counts as deleted. Call [`DeployedCell::delete`] when the test checks what happens
/// after the delete. After [`DeployedCell::set_no_delete`], dropping it leaves the cell deployed.
#[must_use = "dropping it deletes the cell"]
pub struct DeployedCell<B>
where
    B: MyrmicBackend,
{
    myrmic: Myrmic<B>,
    sri: String,
    /// the stderr of the `myrmic deploy` that deployed the cell, `None` if the CLI didn't
    output: Option<String>,
    /// whether the drop leaves the cell deployed ([`Self::set_no_delete`])
    no_delete: bool,
}

impl<B> DeployedCell<B>
where
    B: MyrmicBackend,
{
    /// The cell `sri`, deleted through `myrmic` once dropped. `output` is the stderr of the
    /// `myrmic deploy` that deployed it, `None` if the CLI didn't deploy it.
    pub(crate) fn new(myrmic: Myrmic<B>, sri: String, output: Option<String>) -> Self {
        Self {
            myrmic,
            sri,
            output,
            no_delete: false,
        }
    }

    /// the SRI the cell was deployed under
    pub fn get_sri(&self) -> &str {
        &self.sri
    }

    /// The diagnostic output (stderr) of the `myrmic deploy` that deployed the cell, or `None` if
    /// the cell was deployed through a swarm's sorg client.
    pub fn get_output(&self) -> Option<&str> {
        self.output.as_deref()
    }

    /// run: myrmic send `sri` `command` [`payload` --raw] (see [`Myrmic::send`])
    pub async fn send(&self, command: &str, payload: Option<&[u8]>) -> Result<(), Error> {
        self.myrmic.send(&self.sri, command, payload).await
    }

    /// run: myrmic replicate `sri` --tag `tag`...; every runtime carrying any one of `tags` then
    /// holds a replica of the cell's data
    pub async fn replicate<const N: usize>(&self, tags: [&str; N]) -> Result<(), Error> {
        // without tags, the command only shows the cell's entry
        const { assert!(N > 0, "replicate needs at least one tag") };
        let mut args = vec!["replicate", self.sri.as_str()];
        for tag in tags {
            args.extend(["--tag", tag]);
        }
        self.myrmic.run(&args).await?;
        Ok(())
    }

    /// run: myrmic delete --cell `sri`; returns once the SRI is no longer in `myrmic cells status`
    pub async fn delete(self) -> Result<(), Error> {
        self.myrmic.run(&["delete", "--cell", &self.sri]).await?;
        self.myrmic.wait_until_deployed(&self.sri, false).await
    }

    /// Make dropping the handle leave the cell deployed, until the swarm goes away. For cells
    /// that live as long as the swarm, where deleting each one at the end would only cost time,
    /// e.g. the many cells of a benchmark.
    pub fn set_no_delete(&mut self) {
        self.no_delete = true;
    }
}

impl<B> std::fmt::Debug for DeployedCell<B>
where
    B: MyrmicBackend,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeployedCell")
            .field("sri", &self.sri)
            .finish_non_exhaustive()
    }
}

impl<B> Drop for DeployedCell<B>
where
    B: MyrmicBackend,
{
    fn drop(&mut self) {
        if !self.no_delete {
            match self.myrmic.run_blocking(&["delete", "--cell", &self.sri]) {
                Ok(output) if output.success || already_gone(&output.stderr, &DEPLOYMENT_GONE) => {}
                Ok(output) => eprintln!(
                    "DeployedCell drop-guard: failed to delete cell `{}`: {}",
                    self.sri, output.stderr
                ),
                Err(err) => eprintln!(
                    "DeployedCell drop-guard: failed to run the delete of cell `{}`: {err}",
                    self.sri
                ),
            }
        }
    }
}

/// An app deployed with `myrmic deploy <app-spec>`.
///
/// Dropping a `DeployedApp` deletes the whole app, best effort, so cleanup also happens when the
/// test panics. This also happens after an explicit [`DeployedApp::delete`]; an app that is already
/// gone counts as deleted. Call [`DeployedApp::delete`] when the test checks what happens after the
/// delete.
pub struct DeployedApp<B>
where
    B: MyrmicBackend,
{
    myrmic: Myrmic<B>,
    name: String,
    sris: Vec<String>,
    output: String,
}

impl<B> DeployedApp<B>
where
    B: MyrmicBackend,
{
    pub(super) fn new(myrmic: Myrmic<B>, name: String, sris: Vec<String>, output: String) -> Self {
        Self {
            myrmic,
            name,
            sris,
            output,
        }
    }

    /// the app name every cell of the app is grouped under
    pub fn get_name(&self) -> &str {
        &self.name
    }

    /// the SRIs of the app's cells and bridges, as `myrmic deploy` reported them
    pub fn get_sris(&self) -> &[String] {
        &self.sris
    }

    /// the diagnostic output (stderr) of the `myrmic deploy` that deployed the app
    pub fn get_output(&self) -> &str {
        &self.output
    }

    /// run: myrmic delete `name` --app; returns once none of [`Self::get_sris`] is in
    /// `myrmic cells status` any more
    pub async fn delete(self) -> Result<(), Error> {
        self.myrmic.run(&["delete", &self.name, "--app"]).await?;
        for sri in &self.sris {
            self.myrmic.wait_until_deployed(sri, false).await?;
        }
        Ok(())
    }
}

impl<B> Drop for DeployedApp<B>
where
    B: MyrmicBackend,
{
    fn drop(&mut self) {
        match self.myrmic.run_blocking(&["delete", &self.name, "--app"]) {
            Ok(output) if output.success || already_gone(&output.stderr, &DEPLOYMENT_GONE) => {}
            Ok(output) => eprintln!(
                "DeployedApp drop-guard: failed to delete app `{}`: {}",
                self.name, output.stderr
            ),
            Err(err) => eprintln!(
                "DeployedApp drop-guard: failed to run the delete of app `{}`: {err}",
                self.name
            ),
        }
    }
}
