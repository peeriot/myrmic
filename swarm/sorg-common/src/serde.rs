//! Module defining how sorg plugins serialize and deserialize the data that they exchange via zenoh

use serde::{Serialize, de::DeserializeOwned};
use zenoh::bytes::ZBytes;

use crate::{Error, Result};

/// Trait which must be implemented for anything that is to be used as the payload for messages sent to or by
/// a sorg daemon.
pub trait SorgPayload: Sized {
    fn from_payload(zbytes: &ZBytes, context: &'static str) -> Result<Self> {
        Self::from_slice(zbytes.to_bytes().as_ref(), context)
    }

    fn from_slice(slice: &[u8], context: &'static str) -> Result<Self>;

    fn to_payload(self) -> Result<ZBytes> {
        let bytes = self.to_bytes()?;
        Ok(ZBytes::from(bytes))
    }

    fn to_bytes(self) -> Result<Vec<u8>>;
}

impl<T> SorgPayload for T
where
    T: Serialize + DeserializeOwned + Sized,
{
    fn from_slice(slice: &[u8], context: &'static str) -> Result<Self> {
        let instance =
            postcard::from_bytes::<Self>(slice).map_err(|postcard_err| Error::Postcard {
                context,
                error: postcard_err,
            })?;
        Ok(instance)
    }

    fn to_bytes(self) -> Result<Vec<u8>> {
        postcard::to_allocvec(&self).map_err(|postcard_err| Error::Postcard {
            context: "serialization",
            error: postcard_err,
        })
    }
}

#[cfg(test)]
mod test {
    use serde::{Deserialize, Serialize};

    use super::SorgPayload;

    // FIXME: https://github.com/peeriot/swarm/issues/737
    //        Re-enable this test once we have a separate serde trait
    // #[test]
    // fn deser_is_type_sensitive() {
    //     let depl_id = DeploymentId::default();
    //     let name = "my appl";
    //     let meta_data = Metadata::new(depl_id, name);
    //     let depl_record = DeploymentInitRecord::new(meta_data, vec![], &[]);
    //
    //     let payload = depl_record.to_payload().unwrap();
    //     assert_err!(DeploymentId::from_payload(&payload, "test"));
    // }

    #[derive(Deserialize, Serialize, PartialEq, Debug, Clone, Copy)]
    struct Capabilities;

    #[test]
    fn deser_zero_size_works() {
        let zero_sized = Capabilities;
        let payload = zero_sized.to_payload().unwrap();
        let desered = Capabilities::from_payload(&payload, "test").unwrap();
        assert_eq!(zero_sized, desered);
    }
}
