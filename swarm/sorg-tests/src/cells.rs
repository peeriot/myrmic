use cell_protocol::{AddMode, ArtifactPlatform, ClassArtifact, Sri};
use claims::assert_ok;
use db_client::v1::Client as DbClient;
use sorg_common::{DeploymentError, RequirementTags};

use crate::TestApp;

pub use sorg_client::EventQueue; // re-export to be visible in tests

/// Resolves a test cell name (or UUID string) into an `Sri` the same way the
/// CLI/edge does: a UUID literal is taken verbatim, otherwise the name is an
/// SRN path folded to its deterministic SRI.
fn to_sri(name: &str) -> Sri {
    Sri::from_target(name).expect("invalid cell SRI/name")
}

impl TestApp {
    /// Loads a cell via the sorg client (which routes through orchestration).
    pub async fn deploy_wasm_cell(&self, class_name: impl AsRef<str>, cell_sri: impl AsRef<str>) {
        assert_ok!(self.try_deploy_wasm_cell(class_name, cell_sri).await);
    }

    /// Like `deploy_wasm_cell`, but returns the result instead of asserting.
    pub async fn try_deploy_wasm_cell(
        &self,
        class_name: impl AsRef<str>,
        cell_sri: impl AsRef<str>,
    ) -> Result<(), DeploymentError> {
        self.sorg_client
            .deploy_wasm_cell(
                to_sri(cell_sri.as_ref()),
                class_name.as_ref(),
                RequirementTags::default(),
            )
            .await
    }

    pub async fn undeploy_cell(&self, cell_sri: impl AsRef<str>) {
        assert_ok!(self.try_undeploy_cell(cell_sri).await);
    }

    pub async fn try_undeploy_cell(
        &self,
        cell_sri: impl AsRef<str>,
    ) -> Result<(), sorg_client::Error> {
        self.sorg_client
            .undeploy_cell(to_sri(cell_sri.as_ref()))
            .await
    }

    /// Fire-and-forget command send. Commands no longer return a value; observe
    /// results via `subscribe_cell_event` (the cell publishes) or side effects.
    /// `sri` is a cell name (or UUID string), resolved via `Sri::from_target`.
    pub async fn command_send(
        &self,
        sri: impl AsRef<str>,
        cmd_name: &str,
        payload: Option<Vec<u8>>,
    ) {
        assert_ok!(self.try_command_send(sri, cmd_name, payload).await);
    }

    /// Like `command_send`, but returns the result instead of asserting. Use
    /// when the send is expected to be rejected — e.g. commanding a cell that
    /// has been undeployed, which the client rejects with a "has no placement"
    /// error rather than silently forwarding.
    pub async fn try_command_send(
        &self,
        sri: impl AsRef<str>,
        cmd_name: &str,
        payload: Option<Vec<u8>>,
    ) -> Result<(), sorg_client::Error> {
        self.sorg_client
            .command_send(to_sri(sri.as_ref()), cmd_name, payload, None)
            .await
    }

    pub async fn publish_cell_event(&self, event: &str, payload: Vec<u8>) {
        assert_ok!(
            self.sorg_client
                .publish_cell_event(event, Some(payload))
                .await
        );
    }

    pub async fn subscribe_cell_event(&mut self, event: &str) -> EventQueue {
        assert_ok!(self.sorg_client.subscribe_cell_event(event).await)
    }

    pub async fn register_raw_class(&self, class_name: &str, wasm_bytes: Vec<u8>) {
        assert_ok!(
            sorg_common::class_registry::add_class_artifact(
                self.session(),
                class_name,
                ClassArtifact::Wasm(wasm_bytes),
                AddMode::Strict,
            )
            .await,
        );
    }

    /// Stages a target-specific AOT + meta artifact pair for a class with the
    /// given raw bytes. Used by the embedded deploy tests: the mock embedded
    /// runtime never reads the blobs, so dummy bytes are sufficient — only the
    /// artifact's presence (and target) matters to the orchestrator.
    pub async fn register_raw_aot(
        &self,
        class_name: &str,
        platform: ArtifactPlatform,
        aot_bytes: Vec<u8>,
        meta_bytes: Vec<u8>,
    ) {
        assert_ok!(
            sorg_common::class_registry::add_class_artifact(
                self.session(),
                class_name,
                ClassArtifact::Aot {
                    platform,
                    aot_blob: aot_bytes,
                    meta_blob: meta_bytes,
                },
                AddMode::Force,
            )
            .await,
        );
    }

    pub async fn is_cell_registered(&self, sri: &str) -> bool {
        self.sorg_client
            .placement_exists(&to_sri(sri))
            .await
            .expect("failed to query cell placement")
    }

    /// Reads a value a cell stored in its own private KV space, returning the
    /// raw stored bytes (decode with the cell's codec, e.g. `postcard`).
    ///
    /// The DB scope is resolved exactly as the host does for a cell's default
    /// `Scope` (`transform_scope`): the cell's own database under the cells
    /// namespace (`scope_of_cell`). `key` is the full stored key — for a guest
    /// `Kv::new("p/")` that's `"p/{relative_key}"`, e.g. `"counter/count"`.
    ///
    /// This is how a fire-and-forget command's effect is observed now that
    /// commands no longer return a value: the cell writes the DB, the test
    /// reads it back.
    pub async fn read_cell_kv(&self, cell_sri: impl AsRef<str>, key: &str) -> Option<Vec<u8>> {
        let scope = cell_protocol::scope_of_cell(to_sri(cell_sri.as_ref()));
        let db = DbClient::new(self.session());
        let key = key.to_owned();
        db.read_tx_in(scope.clone(), async move |client, tx_id| {
            Ok(sorg_common::key_get(client.clone(), tx_id, scope, key).await)
        })
        .await
        .expect("failed to open db transaction")
        .expect("failed to read cell kv")
    }
}
