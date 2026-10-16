//! Payloads decoded borrowing from the argument buffer instead of copying out
//! of it.

use myrmic_sdk::{BorrowDecoder, Encoder, borrow_arguments};

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
#[codec(myrmic_sdk::Postcard)]
struct Greeting<'a> {
    name: &'a str,
    data: &'a [u8],
}

#[test]
fn a_derived_message_with_a_lifetime_borrows() {
    let bytes = Greeting {
        name: "ada",
        data: b"xy",
    }
    .to_bytes()
    .unwrap();

    let greeting = Greeting::decode(&bytes).unwrap();

    assert_eq!(
        greeting,
        Greeting {
            name: "ada",
            data: b"xy"
        }
    );
    let buffer = bytes.as_ptr_range();
    assert!(buffer.contains(&greeting.name.as_ptr()));
    assert!(buffer.contains(&greeting.data.as_ptr()));
}

#[test]
fn raw_bytes_and_options_borrow() {
    assert_eq!(<&[u8]>::decode(b"raw").unwrap(), b"raw");
    assert_eq!(Option::<&[u8]>::decode(b"").unwrap(), None);
    assert_eq!(Option::<&[u8]>::decode(b"x").unwrap(), Some(&b"x"[..]));
}

#[test]
fn a_missing_payload_is_reported_as_missing() {
    let err = borrow_arguments::<Greeting<'_>>(&[]).unwrap_err();
    assert!(err.starts_with("no payload was sent"), "{err}");
}
