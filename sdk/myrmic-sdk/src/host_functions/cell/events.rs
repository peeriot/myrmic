use myrmic_common::cells::EventPublishRequest;

use crate::ApiResult;

mod c_functions {
    use core::ffi::c_int;

    #[link(wasm_import_module = "cell")]
    unsafe extern "C" {

        /// Publishes an event. The function is used to publish an event to all cells which subscribed, specified by a payload
        /// which can be deserialized into a `EventPublishRequest`.
        ///
        /// # Arguments:
        /// - buffer: pointer to the memory where the module has the event pub request
        /// - length: length of the serialized event pub request
        ///
        /// # Returns:
        /// - [`crate::SUCCESS`] on success
        /// - [`crate::GENERIC_ERROR`] on failure
        pub(super) fn publish_event(buffer: *const u8, length: c_int) -> c_int;
    }
}

/// Hands a pre-built [`EventPublishRequest`] to the host for delivery to every
/// subscribed cell. Prefer [`publish`](crate::publish), which builds the
/// request and encodes the payload for you.
pub fn publish_event(event: &EventPublishRequest) -> ApiResult {
    crate::host_functions::call(event, c_functions::publish_event).map_err(Into::into)
}
