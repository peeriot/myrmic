use alloc::{borrow::ToOwned, string::String};
use serde::{Deserialize, Serialize};

/// Longest accepted event or command name in bytes. A name becomes the
/// schema of its event scope, so it ends up in zenoh key expressions and db
/// store keys; matches the spawn ref's class-name bound
/// ([`SPAWN_REF_MAX_NAME`](super::spawn_ref::SPAWN_REF_MAX_NAME)).
pub const MAX_NAME_LEN: usize = 128;

/// Represents a validated command identifier. Deserializing validates too.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(try_from = "String")]
pub struct Command(String);

/// Represents a validated event identifier. Deserializing validates too.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(try_from = "String")]
pub struct Event(String);

// generates the convenience impls for the name types
macro_rules! impl_name_type {
    ($ty: ident) => {
        impl $ty {
            #[doc = concat!("Validates `name` and wraps it as a `", stringify!($ty), "`.")]
            pub fn new(name: String) -> Result<Self, &'static str> {
                validate_function_name_component(&name)?;
                Ok(Self(name))
            }
        }

        impl AsRef<str> for $ty {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<&str> for $ty {
            type Error = &'static str;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                $ty::new(value.to_owned())
            }
        }

        impl TryFrom<String> for $ty {
            type Error = &'static str;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                $ty::new(value)
            }
        }

        // Also implement FromStr for ergonomic parsing
        impl alloc::str::FromStr for $ty {
            type Err = &'static str;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                $ty::new(s.to_owned())
            }
        }
    };
}

// generate the impls shared by the name types
impl_name_type!(Command);
impl_name_type!(Event);

/// Validates that a string can be used as a component of a function name (we need this for the command and event names).
/// Since function names will be prefixed (e.g., "command_" or "event_"),
/// the component can start with a number. It is at most [`MAX_NAME_LEN`] bytes.
///
/// Returns `Ok(())` if valid, `Err(&'static str)` with an error message if invalid.
fn validate_function_name_component(input: &str) -> Result<(), &'static str> {
    if input.len() > MAX_NAME_LEN {
        return Err("name is too long");
    }

    // Check for empty string
    if input.is_empty() {
        return Err("name cannot be empty");
    }

    if input.contains(char::is_whitespace) {
        return Err("name cannot contain whitespace");
    }

    // Check that all characters are ASCII alphanumeric or underscore
    if !input.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err("name can only contain ASCII alphanumeric characters and underscores");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Command, Event, MAX_NAME_LEN, validate_function_name_component};
    use crate::cells::EventPublishRequest;
    use alloc::string::String;
    use claims::{assert_err, assert_ok};

    #[test]
    fn deserialize_rejects_every_name_new_rejects() {
        let rejected = [
            String::new(),
            String::from(" "),
            String::from("a b"),
            String::from("/"),
            String::from("a/b"),
            String::from("*"),
            String::from("**"),
            String::from("$*"),
            String::from("x?"),
            String::from("#"),
            String::from("@"),
            String::from(".."),
            String::from("\0"),
            String::from("\u{e9}"),
            "a".repeat(MAX_NAME_LEN + 1),
        ];

        for name in rejected {
            let bytes = postcard::to_allocvec(&name).unwrap();

            assert_err!(Event::new(name.clone()));
            assert_err!(Command::new(name));
            assert_err!(postcard::from_bytes::<Event>(&bytes));
            assert_err!(postcard::from_bytes::<Command>(&bytes));
        }
    }

    #[test]
    fn deserialize_keeps_valid_names_and_their_wire_format() {
        let name = "a".repeat(MAX_NAME_LEN);
        let event = Event::new(name.clone()).unwrap();
        let bytes = postcard::to_allocvec(&event).unwrap();

        // Same bytes as a plain string: old peers and stored rows still decode.
        assert_eq!(bytes, postcard::to_allocvec(&name).unwrap());
        assert_eq!(postcard::from_bytes::<Event>(&bytes).unwrap(), event);
        assert_eq!(
            postcard::from_bytes::<Command>(&bytes).unwrap(),
            Command::new(name).unwrap()
        );
    }

    #[test]
    fn publish_request_from_the_guest_abi_rejects_a_forbidden_name() {
        // The bytes a hostile cell hands the raw `cell.publish_event` import:
        // `EventPublishRequest { event: "x?", payload: None }`.
        assert_err!(postcard::from_bytes::<EventPublishRequest>(&[
            0x02, b'x', b'?', 0x00
        ]));
        assert_ok!(postcard::from_bytes::<EventPublishRequest>(&[
            0x02, b'x', b'y', 0x00
        ]));
    }

    #[test]
    fn validate_function_name_component_valid_cases() {
        // Valid: simple alphanumeric
        assert_ok!(validate_function_name_component("my_event"));
        assert_ok!(validate_function_name_component("MyEvent"));
        assert_ok!(validate_function_name_component("event123"));
        assert_ok!(validate_function_name_component("_event"));
        assert_ok!(validate_function_name_component("event_name_123"));

        // Valid: can start with number (since we use prefix)
        assert_ok!(validate_function_name_component("123event"));
        assert_ok!(validate_function_name_component("0"));

        // Valid: underscores
        assert_ok!(validate_function_name_component("_"));
        assert_ok!(validate_function_name_component("__"));
        assert_ok!(validate_function_name_component(
            "event_name_with_underscores"
        ));
    }

    #[test]
    fn validate_function_name_component_empty_string() {
        assert_err!(validate_function_name_component(""));
    }

    #[test]
    fn validate_function_name_component_whitespace_leading() {
        assert_err!(validate_function_name_component(" event"));
        assert_err!(validate_function_name_component("  event"));
        assert_err!(validate_function_name_component("\tevent"));
        assert_err!(validate_function_name_component("\nevent"));
        assert_err!(validate_function_name_component("\revent"));
    }

    #[test]
    fn validate_function_name_component_whitespace_trailing() {
        assert_err!(validate_function_name_component("event "));
        assert_err!(validate_function_name_component("event  "));
        assert_err!(validate_function_name_component("event\t"));
        assert_err!(validate_function_name_component("event\n"));
        assert_err!(validate_function_name_component("event\r"));
    }

    #[test]
    fn validate_function_name_component_whitespace_middle() {
        assert_err!(validate_function_name_component("my event"));
        assert_err!(validate_function_name_component("my  event"));
        assert_err!(validate_function_name_component("my\tevent"));
        assert_err!(validate_function_name_component("my\nevent"));
        assert_err!(validate_function_name_component("my\revent"));
        assert_err!(validate_function_name_component("my event name"));
    }

    #[test]
    fn validate_function_name_component_special_characters() {
        // Common special characters that can't be in function names
        assert_err!(validate_function_name_component("event!"));
        assert_err!(validate_function_name_component("event@"));
        assert_err!(validate_function_name_component("event#"));
        assert_err!(validate_function_name_component("event$"));
        assert_err!(validate_function_name_component("event%"));
        assert_err!(validate_function_name_component("event^"));
        assert_err!(validate_function_name_component("event&"));
        assert_err!(validate_function_name_component("event*"));
        assert_err!(validate_function_name_component("event("));
        assert_err!(validate_function_name_component("event)"));
        assert_err!(validate_function_name_component("event-"));
        assert_err!(validate_function_name_component("event+"));
        assert_err!(validate_function_name_component("event="));
        assert_err!(validate_function_name_component("event["));
        assert_err!(validate_function_name_component("event]"));
        assert_err!(validate_function_name_component("event{"));
        assert_err!(validate_function_name_component("event}"));
        assert_err!(validate_function_name_component("event|"));
        assert_err!(validate_function_name_component("event\\"));
        assert_err!(validate_function_name_component("event:"));
        assert_err!(validate_function_name_component("event;"));
        assert_err!(validate_function_name_component("event\""));
        assert_err!(validate_function_name_component("event'"));
        assert_err!(validate_function_name_component("event<"));
        assert_err!(validate_function_name_component("event>"));
        assert_err!(validate_function_name_component("event,"));
        assert_err!(validate_function_name_component("event."));
        assert_err!(validate_function_name_component("event?"));
        assert_err!(validate_function_name_component("event/"));
        assert_err!(validate_function_name_component("event~"));
        assert_err!(validate_function_name_component("event`"));
    }

    #[test]
    fn validate_function_name_component_unicode_characters() {
        // Unicode characters that aren't valid in function names
        assert_err!(validate_function_name_component("évént"));
        assert_err!(validate_function_name_component("event中文"));
        assert_err!(validate_function_name_component("event🚀"));
        assert_err!(validate_function_name_component("eventα"));
        assert_err!(validate_function_name_component("event→"));
    }

    #[test]
    fn validate_function_name_component_mixed_invalid() {
        // Multiple issues
        assert_err!(validate_function_name_component(" my event! "));
        assert_err!(validate_function_name_component("event@123"));
        assert_err!(validate_function_name_component("event-name"));
        assert_err!(validate_function_name_component("event.name"));
    }

    #[test]
    fn validate_function_name_component_edge_cases() {
        // Just whitespace
        assert_err!(validate_function_name_component(" "));
        assert_err!(validate_function_name_component("  "));
        assert_err!(validate_function_name_component("\t"));
        assert_err!(validate_function_name_component("\n"));

        // Only special characters
        assert_err!(validate_function_name_component("!@#$"));

        // Length bound
        assert_err!(validate_function_name_component(
            &"a".repeat(MAX_NAME_LEN + 1)
        ));
        assert_ok!(validate_function_name_component(&"a".repeat(MAX_NAME_LEN)));

        // Valid but edge cases
        assert_ok!(validate_function_name_component("a")); // single letter
        assert_ok!(validate_function_name_component("A")); // single uppercase letter
        assert_ok!(validate_function_name_component("1")); // single digit (OK with prefix)
        assert_ok!(validate_function_name_component("_")); // single underscore
    }
}
