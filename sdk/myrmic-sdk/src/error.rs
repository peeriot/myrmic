use core::error::Error;
use core::{ffi::c_int, fmt::Display};

/// Result of a raw host-function call, erring with a structured [`ApiError`].
pub type ApiResult<T = ()> = core::result::Result<T, ApiError>;
/// The SDK's general-purpose result; handlers and codecs err with a static message.
pub type Result<T = ()> = core::result::Result<T, &'static str>;

pub trait ErrorCode {
    fn to_result(self) -> ApiResult;
}

impl ErrorCode for c_int {
    fn to_result(self) -> ApiResult {
        match self {
            0 => Ok(()),
            n => Err(n.into()),
        }
    }
}

/// A structured error returned by the host across the FFI boundary.
#[derive(Debug, Clone, Copy)]
pub enum ApiError {
    /// The host returned an error code this SDK does not know.
    UnknownErrorCode(c_int),
    /// The API was used incorrectly (bad argument or call sequence).
    Usage,
    /// The semantic-store query was rejected as malformed.
    SemQuery,
    /// Payload (de)serialization failed; carries which step failed.
    Serde(&'static str),
    /// The host did not answer within the timeout.
    TimedOut,
    /// The provided scratch buffer cannot hold the payload.
    BufferTooSmall,
    /// The host cannot serve the request yet; retrying later may succeed
    /// (e.g. an embedded host's clock has not synced with the swarm).
    NotReady,
    /// Another cell has claimed exclusive ownership of the signal layer
    /// (swarm#1340); this cell cannot access taps or outlets.
    SignalLayerClaimed,
    /// The handle is not — or is no longer — valid for this operation: the
    /// backing service is gone, the handle predates a reconnect, or it was
    /// never issued. Re-resolve to obtain a fresh handle; retrying with the
    /// same one cannot succeed.
    Unavailable,
    /// The slot's declared wire type does not match the `T` of a typed
    /// read/write (swarm#1315). The value never crossed the boundary.
    TypeMismatch {
        /// The caller's `T::TYPE_ID`.
        expected: u32,
        /// The slot's declared type id.
        actual: u32,
    },
}

impl Error for ApiError {}

impl Display for ApiError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let msg = match self {
            ApiError::UnknownErrorCode(_) => "unknown error code",
            ApiError::Usage => "incorrect api usage",
            ApiError::SemQuery => "incorrect sem query",
            ApiError::Serde(context) => context,
            ApiError::TimedOut => "timed out",
            ApiError::BufferTooSmall => "scratch buffer too small for payload",
            ApiError::NotReady => "not ready, try again",
            ApiError::SignalLayerClaimed => "signal layer claimed by another cell",
            ApiError::Unavailable => "unavailable, re-resolve the handle",
            ApiError::TypeMismatch { .. } => "wire type mismatch between cell and slot",
        };
        write!(f, "{msg}")
    }
}

impl From<c_int> for ApiError {
    fn from(value: c_int) -> Self {
        match value {
            0 => unreachable!("0 is not an error"),
            -11 => ApiError::NotReady,
            -13 => ApiError::SignalLayerClaimed,
            myrmic_common::types::error::ESTALE => ApiError::Unavailable,
            -127 => ApiError::Serde("unable to serialise request"),
            n => ApiError::UnknownErrorCode(n),
        }
    }
}

impl From<ApiError> for &'static str {
    fn from(value: ApiError) -> Self {
        match value {
            ApiError::UnknownErrorCode(_) => "unknown error code",
            ApiError::Usage => "incorrect api usage",
            ApiError::SemQuery => "incorrect sem query",
            ApiError::Serde(context) => context,
            ApiError::TimedOut => "timed out",
            ApiError::BufferTooSmall => "scratch buffer too small for payload",
            ApiError::NotReady => "not ready, try again",
            ApiError::SignalLayerClaimed => "signal layer claimed by another cell",
            ApiError::Unavailable => "unavailable, re-resolve the handle",
            ApiError::TypeMismatch { .. } => "wire type mismatch between cell and slot",
        }
    }
}

#[cfg(test)]
mod tests {
    use myrmic_common::types::error::{EINVAL, ESTALE};

    use super::ApiError;

    #[test]
    fn estale_maps_to_unavailable() {
        assert!(matches!(ApiError::from(ESTALE), ApiError::Unavailable));
    }

    #[test]
    fn known_codes_keep_their_variants() {
        assert!(matches!(ApiError::from(-11), ApiError::NotReady));
        assert!(matches!(ApiError::from(-127), ApiError::Serde(_)));
    }

    #[test]
    fn unassigned_codes_stay_unknown() {
        // -1 is EPERM, deliberately not blessed as Unavailable: the hosts no
        // longer use it for anything a cell should act on.
        assert!(matches!(ApiError::from(-1), ApiError::UnknownErrorCode(-1)));
        assert!(matches!(
            ApiError::from(EINVAL),
            ApiError::UnknownErrorCode(_)
        ));
    }
}
