use std::{ops::Deref, sync::Arc};

use swarm_api::DropSender;
use tokio::task::JoinHandle;
use zenoh::internal::runtime::Runtime;
use zenoh::{Session, Wait};

pub struct SwarmSession {
    pub session: Session,
    pub runtime: Runtime,
    telemetry_guard: Option<Arc<swarm_telemetry::Guard>>,
    telemetry_control_handle: Option<tokio::task::AbortHandle>,
}

impl SwarmSession {
    pub(super) fn new(
        session: Session,
        runtime: Runtime,
        telemetry_guard: Option<Arc<swarm_telemetry::Guard>>,
        telemetry_control_handle: Option<tokio::task::AbortHandle>,
    ) -> Self {
        Self {
            session,
            runtime,
            telemetry_guard,
            telemetry_control_handle,
        }
    }
}

impl Drop for SwarmSession {
    fn drop(&mut self) {
        if let Some(handle) = self.telemetry_control_handle.take() {
            handle.abort();
        }
    }
}

impl Deref for SwarmSession {
    type Target = Session;

    fn deref(&self) -> &Self::Target {
        &self.session
    }
}

// Swarm is `Spawning`, calling `wait` function will wait for the swarm to be
// fulled `Spawned`.
pub struct Spawning {
    pub kill_signal: DropSender,
    pub handle: JoinHandle<SwarmSession>,
}

impl Spawning {
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    pub async fn wait(self) -> anyhow::Result<Spawned> {
        let Self {
            kill_signal,
            handle,
        } = self;

        Ok(Spawned {
            kill_signal,
            kill_on_drop: false,
            session: handle.await?,
        })
    }
}

pub struct Spawned {
    kill_signal: DropSender,
    pub kill_on_drop: bool,
    pub session: SwarmSession,
}

impl Spawned {
    pub fn kill_on_drop(mut self) -> Self {
        self.kill_on_drop = true;
        self
    }

    pub fn session(&self) -> &Session {
        &self.session.session
    }

    pub fn telemetry_guard(&self) -> Option<&swarm_telemetry::Guard> {
        self.session.telemetry_guard.as_deref()
    }

    pub fn kill(&self) {
        if let Err(err) = self.kill_signal.send(()) {
            tracing::warn!("unable to send kill signal, continuing: {}", err);
        }

        let res = self.session.session.close().wait();
        if let Err(err) = res {
            tracing::error!("Unable to close session: {}", err);
        }
        let res = self.session.runtime.close().wait();
        if let Err(err) = res {
            tracing::error!("Unable to close runtime: {}", err);
        }
    }

    pub async fn kill_async(&self) {
        if let Err(err) = self.kill_signal.send(()) {
            tracing::warn!("unable to send kill signal, continuing: {}", err);
        }

        let res = self.session.session.close().await;
        if let Err(err) = res {
            tracing::error!("Unable to close session: {}", err);
        }
        let res = self.session.runtime.close().await;
        if let Err(err) = res {
            tracing::error!("Unable to close runtime: {}", err);
        }
    }
}

impl Drop for Spawned {
    fn drop(&mut self) {
        if self.kill_on_drop {
            self.kill();
        }
    }
}
