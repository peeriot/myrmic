use std::time::Duration;

use zenoh::Session;

use crate::clients::sorg::SorgHandle;

/// A running swarm instance together with a zenoh session connected to it.
///
/// `_teardown` is opaque on purpose: a [`crate::swarm::backend::local::LocalBinary`]-spawned
/// instance owns a local child process (killed on drop via `kill_on_drop`), while a distributed
/// deployment (e.g. SSH-started runtimes across a rack of hosts) owns a guard that stops each
/// remote runtime on drop instead — [`SwarmProcess`]'s own API only ever needs the session, so
/// nothing here has to know which kind of teardown it's holding.
pub struct SwarmProcess {
    _teardown: Box<dyn std::any::Any + Send>,
    session: Session,
    telemetry_files: Option<crate::telemetry_files::TelemetryFiles>,
}

impl SwarmProcess {
    pub(crate) fn new(teardown: impl std::any::Any + Send, session: Session) -> Self {
        Self {
            _teardown: Box::new(teardown),
            session,
            telemetry_files: None,
        }
    }

    /// Marks this swarm as exporting telemetry to per-host files rather than
    /// the db (see `rack::RackTelemetry::Files`), so telemetry queries fetch
    /// over SSH instead of scanning db tables.
    #[must_use]
    pub(crate) fn with_telemetry_files(
        mut self,
        files: crate::telemetry_files::TelemetryFiles,
    ) -> Self {
        self.telemetry_files = Some(files);
        self
    }

    /// Where this swarm's telemetry files live, when it exports telemetry to
    /// files rather than the db.
    pub fn telemetry_files(&self) -> Option<&crate::telemetry_files::TelemetryFiles> {
        self.telemetry_files.as_ref()
    }

    /// the zenoh session connected to this swarm instance
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Wait until this swarm's datalayer answers queries.
    ///
    /// The spawn only waits for the zenoh transport to accept the client, which happens well
    /// before the swarm's plugins have finished starting. Anything that touches the datalayer —
    /// registering a class artifact, creating an instance — fails with "no connected databases"
    /// until the DB plugin has declared its queryables, so callers must pass through here first.
    ///
    /// Panics if the datalayer is still unreachable after `timeout`, since every datalayer-backed
    /// operation after this point would fail anyway.
    pub async fn wait_for_datalayer(&self, timeout: Duration) {
        let ready = crate::wait_until(timeout, crate::wait::DEFAULT_POLL_INTERVAL, || async {
            sorg_common::class_registry::list_classes(&self.session)
                .await
                .is_ok()
        })
        .await;
        assert!(ready, "swarm datalayer not reachable within {timeout:?}");
    }

    /// Force all telemetry providers in every process backing this swarm to flush.
    ///
    /// Every process (one per rack host, or the single local one) independently declares its own
    /// `TOPIC_FORCE_FLUSH` queryable — a plain `session.get` with the default target
    /// (`QueryTarget::BestMatching`) only reaches *one* of them, and taking only the first reply
    /// (as this used to) silently skips flushing the rest. On a single-process local swarm that's
    /// harmless (there's only one to flush), but on a multi-host rack it means at most one host's
    /// telemetry ever gets force-flushed per call — explicitly targeting every matching queryable
    /// (`QueryTarget::All`) and waiting out every reply, like [`swarm_telemetry::query_env_filter`]
    /// already does for the same multi-host reason, is required to actually flush them all.
    ///
    /// A failed flush panics only after the drain, reporting every failing host at once — each
    /// error carries the host label from the replying process plus its replier zid.
    pub async fn force_flush_telemetry(&self) {
        use zenoh::query::{ConsolidationMode, QueryTarget};

        let replies = self
            .session
            .get(swarm_telemetry::TOPIC_FORCE_FLUSH)
            .target(QueryTarget::All)
            .consolidation(ConsolidationMode::None)
            .await
            .expect("failed to query telemetry force-flush endpoint");

        // Drain every reply before judging, collecting errors instead of dying on the first one:
        // with N hosts replying, panicking mid-drain reports one arbitrary failure anonymously
        // and hides whether the other hosts flushed at all — diagnosing the 2026-08-28 rack
        // benchmark failure (two hosts export-dead, the CI log naming neither) took per-node
        // forensics that this message now carries directly. Error replies are labeled host-side
        // (see `swarm_telemetry`'s force-flush queryable); the replier zid covers replies from
        // processes too old to label themselves — e.g. a leftover runtime from a previous pass
        // sharing the mesh, which is exactly what that diagnosis found.
        let mut reply_count = 0;
        let mut errors: Vec<String> = Vec::new();
        let drained = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Ok(reply) = replies.recv_async().await {
                reply_count += 1;
                if let Err(err) = reply.result() {
                    let payload = err.payload().to_bytes();
                    let message =
                        std::str::from_utf8(&payload).unwrap_or("invalid utf-8 error payload");
                    let replier = reply
                        .replier_id()
                        .map_or_else(|| "unknown".to_owned(), |id| id.zid().to_string());
                    errors.push(format!("{message} (replier zid: {replier})"));
                }
            }
        })
        .await;

        assert!(
            errors.is_empty(),
            "telemetry force-flush failed on {}/{reply_count} replying host(s):\n  {}",
            errors.len(),
            errors.join("\n  ")
        );
        assert!(
            drained.is_ok(),
            "timed out waiting for telemetry force-flush replies ({reply_count} host(s) already \
             replied)"
        );
        assert!(
            reply_count > 0,
            "telemetry force-flush query returned no reply"
        );
    }

    /// Blocks until the class registry row for `class` is visible to a routed read from this
    /// session and carries at least one artifact (a wasm hash or a target artifact) — the same
    /// lookup the orchestrator's placement preprocessing does when it decides whether a runtime
    /// can load the cell.
    ///
    /// Class registration (a routed write moments earlier) and the deploy's placement read are
    /// separate transactions with nothing ordering them; the gap used to be papered over by the
    /// driver's own slowness. With the rack nodes on the `performance` cpufreq governor
    /// (2026-08-28), deploys started dying with `MissingArtifact(Wasm)` because the placement
    /// read ran before the registration had converged to whichever node it landed on (runs
    /// 33187972852, 33189664013). Polls the real read path rather than sleeping; times out
    /// after 60s with a warning and proceeds, letting the deploy fail with the precise
    /// placement error.
    pub async fn wait_for_class_visible(&self, class: &str) {
        let start = tokio::time::Instant::now();
        let visible = crate::wait_until(
            std::time::Duration::from_mins(1),
            std::time::Duration::from_millis(250),
            || async {
                sorg_common::class_registry::get_class_info(&self.session, class)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|info| info.wasm_hash.is_some() || !info.artifacts.is_empty())
            },
        )
        .await;

        if !visible {
            eprintln!(
                "warning: class '{class}' still has no visible artifact after {:?}; proceeding \
                 anyway — the deploy will name what is missing",
                start.elapsed()
            );
        } else if start.elapsed() > std::time::Duration::from_millis(300) {
            eprintln!(
                "class '{class}' became visible after {:?} of waiting",
                start.elapsed()
            );
        }
    }

    /// open a [`SorgHandle`] on this swarm (see [`SorgHandle::connect`])
    pub async fn connect_sorg(&self) -> SorgHandle {
        SorgHandle::connect(self.session.clone()).await
    }

    /// open a [`SorgHandle`] scoped to `tags` (see [`SorgHandle::connect_with_tags`])
    pub async fn connect_sorg_with_tags(&self, tags: &[&str]) -> SorgHandle {
        SorgHandle::connect_with_tags(self.session.clone(), tags).await
    }

    /// Opens a [`SorgHandle`] scoped to `tags`, waiting up to `timeout` for the exec runtime
    pub async fn connect_sorg_with_tags_timeout(
        &self,
        tags: &[&str],
        timeout: Duration,
    ) -> SorgHandle {
        SorgHandle::connect_with_tags_timeout(self.session.clone(), tags, timeout).await
    }
}
