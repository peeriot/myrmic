use myrmic_common::cells::{ClassRef, SpawnError, SpawnRequest, TerminateError};
use myrmic_common::types::error::SUCCESS;

use crate::{Encoder, Sri, String, Vec};

mod c_functions {
    use core::ffi::c_int;

    #[link(wasm_import_module = "cell")]
    unsafe extern "C" {
        /// Spawning a cell. The payload is a postcard-serialized `SpawnRequest` specifying the class
        /// reference, local name, and optional placement tags.
        ///
        /// The host derives the child's SRI from the *calling* cell's identity
        /// and the request's `local_name`, then writes the resulting 16-byte
        /// UUID to `out_sri` on success.
        ///
        /// # Arguments:
        /// - buffer: pointer to the memory where the module has the serialized spawn request
        /// - length: length of the serialized spawn request
        /// - out_sri: pointer to a 16-byte buffer the host fills with the child SRI on success
        ///
        /// # Returns:
        /// - [`crate::SUCCESS`] on success
        /// - negative error code on failure (see `SpawnError::try_from`)
        pub(super) fn spawn_cell_host(buffer: *const u8, length: c_int, out_sri: *mut u8) -> c_int;

        /// Terminating a cell. The payload is the UTF-8 encoded SRI of the cell to terminate.
        ///
        /// # Arguments:
        /// - buffer: pointer to the memory where the module has the SRI bytes
        /// - length: length of the SRI in bytes
        ///
        /// # Returns:
        /// - [`crate::SUCCESS`] on success
        /// - negative error code on failure (see `TerminateError::try_from`)
        pub(super) fn terminate_cell_host(buffer: *const u8, length: c_int) -> c_int;

        /// Stops the calling cell and its whole subtree (spec §6). The
        /// parent receives `cell_lost { stopped(code) }`; `code` is passed
        /// only when `code_present` is nonzero.
        ///
        /// # Returns:
        /// - [`crate::SUCCESS`]; the cell dies shortly after this returns
        pub(super) fn stop_self_host(code_present: u32, code: u32) -> c_int;
    }
}

/// Deliberately stops this cell and everything it spawned (minus detached
/// children). The parent — if any — is told `stopped(code)`, letting a
/// supervising parent distinguish clean completion (`None`) from giving up.
/// Returns normally; the host reaps the cell moments later, so treat
/// everything after this call as best-effort cleanup only.
pub fn stop_self(code: Option<u32>) {
    // SAFETY: plain scalar arguments; no guest memory is exchanged.
    unsafe {
        c_functions::stop_self_host(u32::from(code.is_some()), code.unwrap_or_default());
    }
}

/// Spawns a cell of the given class under `request.local_name`, returning the
/// child's SRI as assigned by the host (`child_sri(own_sri, local_name)`). The
/// caller records this to address the child later.
pub fn spawn_cell(request: &SpawnRequest) -> Result<Sri, SpawnError> {
    let payload = postcard::to_allocvec(request).map_err(|_| SpawnError::DeployFailed)?;
    let mut out_sri = [0u8; 16];
    // SAFETY: calling the imported function with pointer and length of guest linear memory,
    // plus a 16-byte out buffer the host fills with the child SRI on success.
    let status = unsafe {
        c_functions::spawn_cell_host(
            payload.as_ptr(),
            payload.len() as core::ffi::c_int,
            out_sri.as_mut_ptr(),
        )
    };
    match status {
        code if code == SUCCESS => Ok(Sri::from_bytes(out_sri)),
        code => Err(SpawnError::try_from(code)
            .unwrap_or_else(|code| panic!("unexpected spawn error code from host: {code}"))),
    }
}

/// A reference to a child cell class, produced by [`crate::declare!`].
///
/// It holds a reference to the module's embedded hash slot, which the toolchain
/// patches at deploy time with the child's SHA-256. The hash is read at spawn
/// time (volatile, so the deploy-time patch is not constant-folded away), never
/// baked at compile time.
#[derive(Clone, Copy)]
pub struct ClassHandle {
    hash: &'static [u8; 32],
}

impl ClassHandle {
    /// Wraps the embedded hash slot. Called by the `declare!` macro; not
    /// intended for direct use.
    #[doc(hidden)]
    pub const fn from_hash_ref(hash: &'static [u8; 32]) -> Self {
        Self { hash }
    }

    /// The child class reference, read from the deploy-patched hash slot.
    pub fn class_ref(&self) -> ClassRef {
        // SAFETY: `hash` points at a 'static [u8; 32] in this module's data
        // segment. The volatile read forces an actual load so the deploy-time
        // patch is observed rather than the compile-time placeholder.
        ClassRef::Hash(unsafe { core::ptr::read_volatile(self.hash as *const [u8; 32]) })
    }

    /// Begins spawning a child of this class: fill the builder out and hand
    /// it off with [`SpawnBuilder::spawn`]. The defaults give an auto-named,
    /// supervised child with cluster-default timings.
    ///
    /// ```
    /// # #![allow(non_snake_case)]
    /// # fn demo(PUMP: myrmic_sdk::ClassHandle, cfg: u32) -> Result<(), myrmic_sdk::SpawnError> {
    /// PUMP.new().name("pump-1").payload(&cfg).spawn()?;
    /// # Ok(())
    /// # }
    /// ```
    #[allow(clippy::new_ret_no_self)]
    pub fn new(&self) -> SpawnBuilder {
        SpawnBuilder {
            handle: *self,
            local_name: None,
            tags: None,
            arguments: None,
            detached: false,
            grace_ms: None,
            deadline_ms: None,
        }
    }

    /// Spawns a child of this class with all defaults —
    /// shorthand for [`new()`](Self::new)`.spawn()`.
    pub fn spawn(&self) -> Result<Sri, SpawnError> {
        self.new().spawn()
    }
}

/// A child spawn being filled out, produced by [`ClassHandle::new`] and
/// handed off by [`spawn`](Self::spawn). Clone one to reuse it as a template.
#[derive(Clone)]
pub struct SpawnBuilder {
    handle: ClassHandle,
    local_name: Option<String>,
    tags: Option<Vec<String>>,
    arguments: Option<Result<Vec<u8>, ()>>,
    detached: bool,
    grace_ms: Option<u64>,
    deadline_ms: Option<u64>,
}

impl SpawnBuilder {
    /// Local name for the child, unique among this cell's children. The
    /// child's SRI is derived from it, so a fixed name makes respawns land
    /// on the same identity. Default: a random name.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.local_name = Some(name.into());
        self
    }

    /// Adds a placement tag constraining where the child may run. Repeatable.
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.get_or_insert_with(Vec::new).push(tag.into());
        self
    }

    /// Payload delivered to the child's `#[init]` handler, encoded via its
    /// [`Encoder`] impl (symmetric with [`send`](crate::send)). An encoding
    /// failure surfaces as `DeployFailed` from [`spawn`](Self::spawn).
    pub fn payload<T: Encoder + ?Sized>(mut self, payload: &T) -> Self {
        self.arguments = Some(payload.to_bytes().map_err(|_| ()));
        self
    }

    /// Decouples the child's lifetime from the spawner's: no fencing against
    /// this cell, excluded from its cascades, no `cell_lost` on either side.
    pub fn detached(mut self) -> Self {
        self.detached = true;
        self
    }

    /// How long the child outlives *this cell's* silence before the runtime
    /// kills it. Longer lets it ride out parent restarts; shorter makes it
    /// die fast with its parent. Default: the cluster TTL.
    pub fn grace(mut self, grace: core::time::Duration) -> Self {
        self.grace_ms = Some(grace.as_millis() as u64);
        self
    }

    /// How long the *child's node* may be silent before the child is
    /// declared dead (rows released, `cell_lost` delivered to the spawner).
    /// Shorter = faster failover but misfires on slow reboots; longer lets
    /// the child ride one out. Default: cluster TTL + margin.
    pub fn deadline(mut self, deadline: core::time::Duration) -> Self {
        self.deadline_ms = Some(deadline.as_millis() as u64);
        self
    }

    /// Hands the spawn to the host, returning the child's SRI.
    pub fn spawn(self) -> Result<Sri, SpawnError> {
        let arguments = match self.arguments {
            None => None,
            Some(Ok(bytes)) => Some(bytes),
            Some(Err(())) => return Err(SpawnError::DeployFailed),
        };
        spawn_cell(&SpawnRequest {
            class: self.handle.class_ref(),
            local_name: self.local_name,
            tags: self.tags,
            arguments,
            detached: self.detached,
            grace_ms: self.grace_ms,
            deadline_ms: self.deadline_ms,
        })
    }
}

/// Requests the host terminate the cell identified by `sri`.
pub fn terminate_cell(sri: &str) -> Result<(), TerminateError> {
    let bytes = sri.as_bytes();
    // SAFETY: calling the imported function with pointer and length of guest linear memory.
    let status = unsafe {
        c_functions::terminate_cell_host(bytes.as_ptr(), bytes.len() as core::ffi::c_int)
    };
    match status {
        code if code == SUCCESS => Ok(()),
        code => Err(TerminateError::try_from(code)
            .unwrap_or_else(|code| panic!("unexpected terminate error code from host: {code}"))),
    }
}
