use anyhow::Context;
use myrmic_common::codegen::bridge_api::{UserHttpBridgeApi, UserMqttBridge};

macro_rules! try_from_yaml_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $(
                $variant:ident($ty:ty)
            ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(::serde::Deserialize)]
        $vis enum $name {
            $( $variant($ty) ),+
        }

        impl ::std::str::FromStr for $name {
            type Err = ::anyhow::Error;

            fn from_str(content: &str) -> ::std::result::Result<Self, Self::Err> {
                let mut found: ::std::option::Option<(Self, &'static str)> = None;
                let mut errs: ::std::vec::Vec<String> = ::std::vec::Vec::new();

                $(
                    let name = stringify!($ty);
                    match ::serde_yaml::from_str::<$ty>(content) {
                        Ok(v) => match found {
                            Some((_, other)) => panic!("[internal error] type overlap: {} vs {}", other, name),
                            None    => found = Some((Self::$variant(v), name)),
                        },
                        Err(e) => errs.push(e.to_string()),
                    }
                )+

                found.map(|(value, _name)| value).ok_or_else(|| ::anyhow::anyhow!("{}", errs.join(" OR ")))
            }
        }
    };
}

try_from_yaml_enum! {
    pub enum ImportInput {
        Mqtt(UserMqttBridge),
        Http(UserHttpBridgeApi),
    }
}

pub fn parse_from_file<T: std::str::FromStr<Err = anyhow::Error>>(
    path: &std::path::Path,
) -> anyhow::Result<T> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("unable to read: {}", path.display()))?;

    T::from_str(&content).with_context(|| format!("unable to parse file: {}", path.display()))
}
