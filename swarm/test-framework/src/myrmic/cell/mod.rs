//! Cell sources and deployed-cell and -app handles for the myrmic shim.

use std::path::{Path, PathBuf};

use crate::myrmic::{DEPLOYMENT_GONE, Error, Myrmic, MyrmicBackend, already_gone};

/// Where a cell's sources live on the target (the machine the CLI runs on): an existing path, or
/// a temporary directory (e.g. created by `myrmic new`) that is removed on drop.
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
/// dropped. A host-side temporary directory would not do: on the ssh and docker backends the CLI
/// writes into the target's file system.
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
    /// without a working `mktemp` is broken test infrastructure, so a failure panics.
    pub(crate) async fn create(myrmic: &Myrmic<B>, prefix: &str) -> Self {
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

/// a deployed cell is the outcome of `myrmic deploy --name <srn> <cell>` (see [`Myrmic::cell`])
///
/// Dropping a `DeployedCell` deletes it best-effort (panic-safe cleanup), also after an explicit
/// [`DeployedCell::delete`]: a cell that is already gone counts as deleted. Call
/// [`DeployedCell::delete`] when the test asserts on the post-delete state.
pub struct DeployedCell<B>
where
    B: MyrmicBackend,
{
    myrmic: Myrmic<B>,
    sri: String,
    output: String,
}

impl<B> DeployedCell<B>
where
    B: MyrmicBackend,
{
    pub(crate) fn new(myrmic: Myrmic<B>, sri: String, output: String) -> Self {
        Self {
            myrmic,
            sri,
            output,
        }
    }

    /// the SRI the cell was deployed under
    pub fn sri(&self) -> &str {
        &self.sri
    }

    /// the diagnostic output (stderr) of the `myrmic deploy` that deployed the cell
    pub fn output(&self) -> &str {
        &self.output
    }

    /// run: myrmic send `sri` `command` (see [`Myrmic::send`])
    pub async fn send(&self, command: &str) -> Result<(), Error> {
        self.myrmic.send(&self.sri, command).await
    }

    /// run: myrmic delete --cell `sri`; returns once the SRI is no longer in `myrmic cells status`
    pub async fn delete(self) -> Result<(), Error> {
        self.myrmic.run(&["delete", "--cell", &self.sri]).await?;
        self.myrmic.wait_until_deployed(&self.sri, false).await
    }
}

impl<B> Drop for DeployedCell<B>
where
    B: MyrmicBackend,
{
    fn drop(&mut self) {
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

/// a deployed app is the outcome of `myrmic deploy <app-spec>`
///
/// Dropping a `DeployedApp` deletes the whole app best-effort (panic-safe cleanup), also after an
/// explicit [`DeployedApp::delete`]: an app that is already gone counts as deleted. Call
/// [`DeployedApp::delete`] when the test asserts on the post-delete state.
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
    pub(crate) fn new(myrmic: Myrmic<B>, name: String, sris: Vec<String>, output: String) -> Self {
        Self {
            myrmic,
            name,
            sris,
            output,
        }
    }

    /// the app name every cell of the app is grouped under
    pub fn name(&self) -> &str {
        &self.name
    }

    /// the SRIs of the app's cells and bridges, as `myrmic deploy` reported them
    pub fn sris(&self) -> &[String] {
        &self.sris
    }

    /// the diagnostic output (stderr) of the `myrmic deploy` that deployed the app
    pub fn output(&self) -> &str {
        &self.output
    }

    /// run: myrmic delete `name` --app; returns once none of [`Self::sris`] is in
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
