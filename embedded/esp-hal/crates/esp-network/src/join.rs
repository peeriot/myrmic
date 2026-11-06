//! Wi-Fi join bookkeeping: why a join keeps failing, and how long to wait
//! before trying again.

/// What the radio reported about a failed join, reduced to what the verdict
/// depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasonClass {
    /// No access point of the configured name (and a compatible security
    /// mode) answered.
    NoAccessPoint,
    /// An access point answered, but authentication or the key handshake
    /// failed.
    Authentication,
    /// Anything else, driver errors included.
    Other,
}

/// The station's link to its network, as the connection task reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// Joined to the access point.
    Joined,
    /// Not joined; with why joining fails, once there is a verdict.
    Down(Option<JoinFailure>),
}

/// Why this board is not on its network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum JoinFailure {
    /// No access point of that name is in range.
    NetworkNotFound = 1,
    /// The access point is in range and rejects the password.
    PasswordRejected = 2,
    /// Something else; the driver's reason is in the log.
    Other = 3,
}

impl JoinFailure {
    /// A nonzero code for storing the failure in an atomic.
    #[must_use]
    pub const fn code(self) -> u8 {
        self as u8
    }

    /// The failure a [`code`](Self::code) stands for; `None` for zero or an
    /// unknown code.
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::NetworkNotFound),
            2 => Some(Self::PasswordRejected),
            3 => Some(Self::Other),
            _ => None,
        }
    }
}

/// The verdict on a failed join, or `None` while it is too early to give one.
///
/// A network missing from the scan is reported at once, whatever the driver's
/// reason. A single authentication failure is not a verdict: the driver reports
/// one on every reboot while the access point still holds the previous session,
/// so a password only counts as rejected once that reason repeats with the
/// network in range.
#[must_use]
pub fn classify(
    reason: ReasonClass,
    ssid_seen: bool,
    consecutive_auth_failures: u8,
) -> Option<JoinFailure> {
    if !ssid_seen {
        return Some(JoinFailure::NetworkNotFound);
    }
    match reason {
        ReasonClass::NoAccessPoint => Some(JoinFailure::NetworkNotFound),
        ReasonClass::Authentication => {
            (consecutive_auth_failures >= 2).then_some(JoinFailure::PasswordRejected)
        }
        ReasonClass::Other => Some(JoinFailure::Other),
    }
}

/// The wait between join attempts: it doubles from 5 s up to 60 s while
/// attempts fail, and a successful join takes it back to 5 s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    next_secs: u64,
}

impl Backoff {
    /// The wait after the first failure, in seconds.
    pub const BASE_SECS: u64 = 5;
    /// The longest wait, in seconds.
    pub const CAP_SECS: u64 = 60;

    /// A schedule at its first wait.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            next_secs: Self::BASE_SECS,
        }
    }

    /// The wait before the next attempt, in seconds; each call doubles the
    /// one after it, up to the cap.
    pub fn next_wait(&mut self) -> u64 {
        let wait = self.next_secs;
        self.next_secs = (wait * 2).min(Self::CAP_SECS);
        wait
    }

    /// Back to the first wait, after a successful join.
    pub fn reset(&mut self) {
        self.next_secs = Self::BASE_SECS;
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}
