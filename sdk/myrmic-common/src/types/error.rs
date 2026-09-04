/// Return value indicating a host function completed successfully.
pub const SUCCESS: i32 = 0;

/// Generic, unspecified failure returned by a host function when no more
/// specific error code applies.
pub const GENERIC_ERROR: i32 = i32::MIN;

// Adopted from errno: the negated POSIX error numbers.

/// Operation not permitted (POSIX `EPERM`).
pub const EPERM: i32 = -1;

/// Resource temporarily unavailable, try again (POSIX `EAGAIN`).
pub const EAGAIN: i32 = -11;

/// Permission denied (POSIX `EACCES`).
pub const EACCES: i32 = -13;

/// Invalid argument (POSIX `EINVAL`).
pub const EINVAL: i32 = -22;

/// Operation timed out (POSIX `ETIMEDOUT`).
pub const ETIMEDOUT: i32 = -110;

/// Stale handle (POSIX `ESTALE`): the handle is not — or is no longer — valid
/// for this operation. Raised when the backing service is gone, the handle
/// predates a reconnect, or it was never issued. Re-resolve to obtain a fresh
/// handle; retrying with the same one cannot succeed.
pub const ESTALE: i32 = -116;
