use alloc::{borrow::ToOwned, string::String};
use serde::{Deserialize, Serialize};

/// Represents a validated command identifier
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Command(String);

/// Represents a validated event identifier
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
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
/// the component can start with a number.
///
/// Returns `Ok(())` if valid, `Err(&'static str)` with an error message if invalid.
fn validate_function_name_component(input: &str) -> Result<(), &'static str> {
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

    // Note to run tests in this crate, we need to run them with `cargo test --target x86_64-unknown-linux-gnu -Z build-std=`

    use super::validate_function_name_component;
    use claims::{assert_err, assert_ok};

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

        // Valid but edge cases
        assert_ok!(validate_function_name_component("a")); // single letter
        assert_ok!(validate_function_name_component("A")); // single uppercase letter
        assert_ok!(validate_function_name_component("1")); // single digit (OK with prefix)
        assert_ok!(validate_function_name_component("_")); // single underscore
    }
}
