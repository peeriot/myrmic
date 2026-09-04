use core::ffi::c_int;

use myrmic_common::cells::{
    CommandRequest, ERR_CELL_CMD_CELL_ERROR, ERR_CELL_CMD_CELL_NOT_PRESENT,
    ERR_CELL_CMD_COMMAND_NOT_PRESENT, ERR_CELL_CMD_INTERNAL, ERR_CELL_CMD_SMALL_BUFFER,
    ERR_CELL_CMD_TIMEOUT,
};

mod c_functions {
    use core::ffi::c_int;

    #[link(wasm_import_module = "cell")]
    unsafe extern "C" {

        /// Sending a command. The function is used to send a command to a cell, both specified via the payload
        /// which can be deserialized into a `CommandRequest`.
        ///
        /// # Arguments:
        /// - buffer: pointer to the memory where the module has the command request
        /// - length: length of the serialized command request
        ///
        /// # Returns:
        /// - [`crate::SUCCESS`] on success
        /// - [`crate::GENERIC_ERROR`] on failure
        pub(super) fn send_command(buffer: *const u8, length: c_int) -> c_int;
    }
}

pub fn send_command(req: &CommandRequest) -> Result<(), CommandError> {
    crate::host_functions::call(req, c_functions::send_command).map_err(Into::into)
}

/// Why sending a command to a cell failed.
#[derive(Copy, Clone)]
pub enum CommandError {
    /// No response received within the timeout
    Timeout,
    /// The indicated cell is not present in the system (TODO we need some kind of cell registry for this)
    CellNotPresent,
    /// The cell exists, but either does not have the specified command or the provided arguments are invalid (e.g., arguments were provided to a cell not expecting input or vice versa)
    ApiError,
    /// The receiving cell crashed/errored out while processing the query
    CellError,
    /// Internal errors within the framework
    Internal,
    /// The provided buffer was too small for the response
    SmallBuffer,
    /// Catchall for other error codes (should not be used)
    Other(i32),
}

impl CommandError {
    /// A short human-readable description of the error (also its
    /// [`Display`](core::fmt::Display) output).
    pub fn describe(&self) -> &'static str {
        match self {
            CommandError::Timeout => "Command timed out",
            CommandError::CellNotPresent => "Target cell not present",
            CommandError::ApiError => "Command not present",
            CommandError::CellError => "Cell error",
            CommandError::Internal => "Internal error",
            CommandError::SmallBuffer => "Buffer too small",
            CommandError::Other(_) => "Other(_)",
        }
    }
}

impl From<c_int> for CommandError {
    fn from(value: c_int) -> Self {
        match value {
            ERR_CELL_CMD_TIMEOUT => Self::Timeout,
            ERR_CELL_CMD_CELL_NOT_PRESENT => Self::CellNotPresent,
            ERR_CELL_CMD_COMMAND_NOT_PRESENT => Self::ApiError,
            ERR_CELL_CMD_CELL_ERROR => Self::CellError,
            ERR_CELL_CMD_INTERNAL => Self::Internal,
            ERR_CELL_CMD_SMALL_BUFFER => Self::SmallBuffer,
            n => Self::Other(n),
        }
    }
}

impl core::fmt::Display for CommandError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.describe())
    }
}
