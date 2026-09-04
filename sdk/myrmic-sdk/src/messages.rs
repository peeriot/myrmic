use crate::{Bytes, Decoder, Encoder, Result, Sri};

use alloc::string::String;
use core::marker::PhantomData;
use myrmic_common::cells::Command;

/// A handler that can be targeted by name.
///
/// The `#[cmd]` macro generates a zero-sized marker type per command handler
/// and implements this on it, so a handler can be referred to at the type level.
/// `NAME` is the wire name (respecting `#[cmd(name = "...")]`); `Arg` is the
/// payload the handler decodes. `#[evt]` does not generate a marker: event
/// handlers are pub/sub and can never be [`Callback`] targets.
pub trait Handler {
    /// The handler's wire name (respecting `#[cmd(name = "...")]`).
    const NAME: &'static str;
    /// The payload type the handler decodes.
    type Arg;
}

/// The command a caller wants a result returned to.
///
/// Commands are fire-and-forget, so a handler can't reply through a return
/// value. Instead the caller sends a `Callback` naming the command to invoke
/// back on the caller's [`Sri`]. `T` is the payload that target expects, so
/// [`invoke`](Callback::invoke) is type-checked against it.
pub struct Callback<T> {
    command: Command,
    _payload: PhantomData<T>,
}

impl<T> Callback<T> {
    /// Build a callback targeting a handler by its generated marker type; the
    /// payload type is taken from the handler, so it can't disagree.
    ///
    /// `Callback::of::<on_reply>()`
    pub fn of<H: Handler<Arg = T>>() -> Self {
        // `H::NAME` originates from `#[cmd]`, which only accepts names that are
        // valid command identifiers.
        Self {
            command: Command::new(H::NAME.into()).expect("handler name is validated by #[cmd]"),
            _payload: PhantomData,
        }
    }

    /// Build a callback from a command name, with the payload type given
    /// explicitly. Use when no handler marker is in scope.
    pub fn to(name: impl Into<String>) -> Result<Self> {
        Ok(Self {
            command: Command::new(name.into())?,
            _payload: PhantomData,
        })
    }
}

impl<T: Encoder> Callback<T> {
    /// Invoke the callback on `sri`, sending `value` as its payload.
    pub fn invoke(self, sri: Sri, value: &T) -> Result<()> {
        crate::send(sri, self, value)
    }
}

impl<T> From<Callback<T>> for Command {
    fn from(value: Callback<T>) -> Self {
        value.command
    }
}

impl<T> Encoder for Callback<T> {
    fn to_bytes(&self) -> Result<Bytes> {
        Ok(self.command.as_ref().as_bytes().to_vec())
    }
}

impl<T> Decoder for Callback<T> {
    fn from_bytes(bytes: Bytes) -> Result<Self> {
        let name = core::str::from_utf8(&bytes).map_err(|_| "callback name is not valid utf-8")?;
        Self::to(name)
    }
}

impl<T> core::fmt::Debug for Callback<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("Callback").field(&self.command).finish()
    }
}

mod serde_json {
    use crate::{Bytes, Decoder, Encoder, Result};
    use alloc::string::String;
    use serde_json::{Map, Number, Value};

    macro_rules! json_codec {
        ($ty:ty, $noun:literal) => {
            impl Decoder for $ty {
                fn from_bytes(bytes: Bytes) -> Result<Self> {
                    serde_json::from_slice(&bytes)
                        .map_err(|_| concat!("payload is not a valid json ", $noun))
                }
            }

            impl Encoder for $ty {
                fn to_bytes(&self) -> Result<Bytes> {
                    serde_json::to_vec(self)
                        .map_err(|_| concat!("unable to serialise json ", $noun))
                }
            }
        };
    }

    json_codec!(Value, "value");
    json_codec!(Number, "number");
    json_codec!(Map<String, Value>, "object");
}
