use zenoh::Session;

use crate::config::Config;

/// Client for the interaction with the self-organization layer of swarm.
#[derive(Debug, Clone)]
pub struct Client {
    pub(crate) config: Config,
    session: Session,
}

impl Client {
    /// Creates a new sorg client which will operate through the
    /// provided zenoh session. The caller of this method has to ensure that
    /// the provided session enables communicating to the sorg orchestration
    /// runtimes of the system that is to be controlled via the client
    #[must_use]
    pub fn new(session: Session) -> Self {
        Self {
            session,
            config: Config::default(),
        }
    }

    /// Creates a new sorg client which will operate through the
    /// provided zenoh session and configuration. The caller of this method has to ensure that
    /// the provided session enables communicating to the sorg orchestration
    /// runtimes of the system that is to be controlled via the client
    #[must_use]
    pub fn new_with_config(session: Session, config: Config) -> Self {
        Self { config, session }
    }

    /// Returns a reference to the session that the client is using for communicating with the
    /// swarm
    #[must_use]
    pub fn session(&self) -> &Session {
        &self.session
    }
}
