/// A parsed URL, borrowing its components from the input string.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(Eq, PartialEq))]
pub struct Url<'a> {
    scheme: Scheme,
    host: &'a str,
    is_host_ipv6: bool,
    scope_id: Option<u32>,
    port: Option<u16>,
    path: &'a str,
    query: Option<&'a str>,
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum ParseError<'a> {
    NoScheme,
    UnsupportedScheme(&'a str),
    Ipv6AddressInvalid(&'a str),
    EmptyHost,
    MissingPort,
    InvalidPort(&'a str),
    MissingScope,
    InvalidScope(&'a str),
}

impl<'a> ParseError<'a> {
    pub fn as_text(&self) -> &'static str {
        match self {
            ParseError::NoScheme => "no scheme provided",
            ParseError::UnsupportedScheme(_) => "unsupported scheme",
            ParseError::Ipv6AddressInvalid(_) => "ipv6 address was invalid",
            ParseError::EmptyHost => "empty host",
            ParseError::MissingPort => "missing port",
            ParseError::InvalidPort(_) => "invalid port",
            ParseError::MissingScope => "missing scope",
            ParseError::InvalidScope(_) => "invalid scope",
        }
    }

    #[allow(clippy::wrong_self_convention)]
    pub fn to_text(&self) -> alloc::string::String {
        match self {
            ParseError::NoScheme => alloc::string::String::from("no scheme provided"),
            ParseError::UnsupportedScheme(value) => alloc::format!("unsupported scheme: {}", value),
            ParseError::Ipv6AddressInvalid(value) => {
                alloc::format!("ipv6 address was invalid: {}", value)
            }
            ParseError::EmptyHost => alloc::string::String::from("host was empty"),
            ParseError::MissingPort => alloc::string::String::from("missing port"),
            ParseError::InvalidPort(port) => alloc::format!("invalid port: {}", port),
            ParseError::MissingScope => alloc::string::String::from("missing scope"),
            ParseError::InvalidScope(scope) => alloc::format!("invalid scope: {}", scope),
        }
    }
}

impl<'a> Url<'a> {
    /// Parse the provided url
    ///
    /// The host may be an IP address. An IPv6 address has to be surrounded by square brackets.
    pub fn parse(url: &'a str) -> Result<Url<'a>, ParseError<'a>> {
        let (scheme, rest) = url.split_once("://").ok_or(ParseError::NoScheme)?;

        let scheme = match scheme.as_bytes() {
            b if b.eq_ignore_ascii_case(b"http") => Scheme::Http,
            b if b.eq_ignore_ascii_case(b"https") => Scheme::Https,
            b if b.eq_ignore_ascii_case(b"mqtt") => Scheme::Mqtt,
            b if b.eq_ignore_ascii_case(b"mqtts") => Scheme::Mqtts,
            _ => return Err(ParseError::UnsupportedScheme(scheme)),
        };

        // Split authority and path+query
        let (authority, path_query) = match rest.find('/') {
            Some(i) => rest.split_at(i),
            None => (rest, ""),
        };

        // Split path and query (handles both "/path?query" and "?query")
        let (path, query) = match path_query.find('?') {
            Some(i) => {
                let (p, q) = path_query.split_at(i);
                (if p.is_empty() { "/" } else { p }, Some(&q[1..]))
            }
            None => (
                if path_query.is_empty() {
                    "/"
                } else {
                    path_query
                },
                None,
            ),
        };

        // Parse authority: [host:port] or host:port
        let (host, port, is_ipv6, scope_id) = if authority.starts_with('[') {
            // IPv6: [addr%scope]:port or [addr]:port or [addr]
            let close = authority
                .find(']')
                .ok_or(ParseError::Ipv6AddressInvalid(authority))?;

            // Extract scope ID if present
            let (addr_end, scope_id_str) = match authority[1..close].find('%') {
                Some(pos) => {
                    let scope_start = pos + 1;
                    (scope_start, Some(&authority[scope_start + 1..close]))
                }
                None => (close, None),
            };

            let host = &authority[1..addr_end];
            if host.is_empty() {
                return Err(ParseError::EmptyHost);
            }
            // Check remainder after closing bracket
            let remainder = &authority[close + 1..];

            // Extract port if present
            let port_str = if remainder.is_empty() {
                None
            } else if let Some(after_colon) = remainder.strip_prefix(':') {
                Some(after_colon)
            } else {
                return Err(ParseError::InvalidPort(remainder));
            };

            (host, port_str, true, scope_id_str)
        } else {
            // IPv4 or hostname: host:port or host
            match authority.split_once(':') {
                Some((host, port)) => (host, Some(port), false, None),
                None => (authority, None, false, None),
            }
        };

        // Validate and parse port
        let port = port
            .map(|p| {
                if p.is_empty() {
                    Err(ParseError::MissingPort)
                } else {
                    p.parse::<u16>().map_err(|_| ParseError::InvalidPort(p))
                }
            })
            .transpose()?;

        // Validate and parse scope ID
        let scope_id = scope_id
            .map(|s| {
                if s.is_empty() {
                    Err(ParseError::MissingScope)
                } else {
                    s.parse::<u32>().map_err(|_| ParseError::InvalidScope(s))
                }
            })
            .transpose()?;

        Ok(Self {
            scheme,
            host,
            is_host_ipv6: is_ipv6,
            scope_id,
            port,
            path,
            query,
        })
    }

    /// Get the url scheme
    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    /// Get the url host
    pub fn host(&self) -> &'a str {
        self.host
    }

    /// Attempt to get the url host as an IP address
    ///
    /// This will only work, if the url host was actually specified as an IP address.
    pub fn host_ip(&self) -> Option<core::net::IpAddr> {
        use core::str::FromStr;

        if self.is_host_ipv6 {
            core::net::Ipv6Addr::from_str(self.host)
                .ok()
                .map(|ip| ip.into())
        } else {
            core::net::Ipv4Addr::from_str(self.host)
                .ok()
                .map(|ip| ip.into())
        }
    }

    /// Attempt to get the url host socket address
    ///
    /// This will only work, if the url host was an IP address
    pub fn host_socket_address(&self) -> Option<core::net::SocketAddr> {
        use core::net::{IpAddr, SocketAddr, SocketAddrV4, SocketAddrV6};

        Some(match self.host_ip()? {
            IpAddr::V4(address) => {
                SocketAddr::V4(SocketAddrV4::new(address, self.port_or_default()))
            }
            IpAddr::V6(address) => SocketAddr::V6(SocketAddrV6::new(
                address,
                self.port_or_default(),
                0,
                self.scope_id_or_default(),
            )),
        })
    }

    /// Get the url port if specified
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// Get the url port or the default port for the scheme
    pub fn port_or_default(&self) -> u16 {
        self.port.unwrap_or_else(|| self.scheme.default_port())
    }

    /// Get the scope ID of the IPv6 address specified in the url
    pub fn scope_id(&self) -> Option<u32> {
        self.scope_id
    }

    /// Get the scope ID of the IPv6 address specified in the url or the default scope ID
    pub fn scope_id_or_default(&self) -> u32 {
        self.scope_id.unwrap_or(0)
    }

    /// Get the url path
    pub fn path(&self) -> &'a str {
        self.path
    }

    /// Get the url query if specified
    pub fn query(&self) -> Option<&'a str> {
        self.query
    }
}

// Add Display impl for serialization
impl<'a> core::fmt::Display for Url<'a> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}://", self.scheme.as_str())?;

        if self.is_host_ipv6 {
            write!(f, "[{}", self.host)?;
            if let Some(scope) = self.scope_id {
                write!(f, "%{}", scope)?;
            }
            write!(f, "]")?;
        } else {
            write!(f, "{}", self.host)?;
        }

        if let Some(port) = self.port {
            write!(f, ":{}", port)?;
        }

        write!(f, "{}", self.path)?;

        if let Some(query) = self.query {
            write!(f, "?{}", query)?;
        }

        Ok(())
    }
}

/// The scheme of a [`Url`].
#[derive(Debug, PartialEq, Eq, Copy, Clone, serde::Serialize, serde::Deserialize)]
pub enum Scheme {
    /// Plain HTTP.
    Http,
    /// HTTP over TLS.
    Https,
    /// Plain MQTT.
    Mqtt,
    /// MQTT over TLS.
    Mqtts,
}

impl Scheme {
    /// str representation of the scheme
    ///
    /// The returned str is always lowercase
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::Mqtt => "mqtt",
            Self::Mqtts => "mqtts",
        }
    }

    /// Get the default port for scheme
    pub const fn default_port(&self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
            Self::Mqtt => 1883,
            Self::Mqtts => 8883,
        }
    }
}

#[cfg(feature = "url-serde")]
pub mod url_serde {
    use alloc::string::ToString;
    // Custom serialization module
    use serde::de::Error;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(url: &super::Url<'_>, ser: S) -> Result<S::Ok, S::Error> {
        url.to_string().serialize(ser)
    }

    pub fn deserialize<'de, 'a, D: Deserializer<'de>>(de: D) -> Result<super::Url<'a>, D::Error>
    where
        'de: 'a, // Input data must outlive the borrow
    {
        let s = <&'a str>::deserialize(de)?;
        super::Url::parse(s).map_err(|err| D::Error::custom(err.as_text()))
    }
}

#[cfg(test)]
mod tests {
    // Required to link `std` for the `std::format!` calls in a `no_std`
    // crate's tests; the `unused_extern_crates` lint misfires on it.
    #[allow(unused_extern_crates)]
    extern crate std;

    use super::*;
    use core::net::SocketAddr;
    use core::str::FromStr;

    #[test]
    fn test_parse_no_scheme() {
        assert_eq!(ParseError::NoScheme, Url::parse("").err().unwrap());
        assert_eq!(ParseError::NoScheme, Url::parse("http:/").err().unwrap());
    }

    #[test]
    fn test_parse_unsupported_scheme() {
        assert_eq!(
            ParseError::UnsupportedScheme("something"),
            Url::parse("something://").err().unwrap()
        );
    }

    #[test]
    fn test_parse_no_host() {
        let url = Url::parse("http://").unwrap();
        assert_eq!(url.scheme(), Scheme::Http);
        assert_eq!(url.host(), "");
        assert_eq!(url.port_or_default(), 80);
        assert_eq!(url.path(), "/");
        assert_eq!(url.query(), None);

        let url = Url::parse("mqtt://").unwrap();
        assert_eq!(url.scheme(), Scheme::Mqtt);
        assert_eq!(url.host(), "");
        assert_eq!(url.port_or_default(), 1883);
        assert_eq!(url.path(), "/");
        assert_eq!(url.query(), None);
    }

    #[test]
    fn test_parse_minimal() {
        let url = Url::parse("http://localhost").unwrap();
        assert_eq!(url.scheme(), Scheme::Http);
        assert_eq!(url.host(), "localhost");
        assert_eq!(url.port_or_default(), 80);
        assert_eq!(url.path(), "/");

        assert_eq!("http://localhost/", std::format!("{}", url));
    }

    #[test]
    fn test_parse_path() {
        let url = Url::parse("http://localhost/foo/bar").unwrap();
        assert_eq!(url.scheme(), Scheme::Http);
        assert_eq!(url.host(), "localhost");
        assert_eq!(url.port_or_default(), 80);
        assert_eq!(url.path(), "/foo/bar");

        assert_eq!("http://localhost/foo/bar", std::format!("{}", url));
    }

    #[test]
    fn test_parse_path_with_colon() {
        let url = Url::parse("http://localhost/foo/bar:123").unwrap();
        assert_eq!(url.scheme(), Scheme::Http);
        assert_eq!(url.host(), "localhost");
        assert_eq!(url.port_or_default(), 80);
        assert_eq!(url.path(), "/foo/bar:123");

        assert_eq!("http://localhost/foo/bar:123", std::format!("{}", url));
    }

    #[test]
    fn test_parse_path_query() {
        let url = Url::parse("mqtt://localhost/foo/bar?foo=bar&hello=world").unwrap();
        assert_eq!(url.scheme(), Scheme::Mqtt);
        assert_eq!(url.host(), "localhost");
        assert_eq!(url.port_or_default(), 1883);
        assert_eq!(url.path(), "/foo/bar");
        assert_eq!(url.query(), Some("foo=bar&hello=world"));

        assert_eq!(
            "mqtt://localhost/foo/bar?foo=bar&hello=world",
            std::format!("{}", url)
        );
    }

    #[test]
    fn test_parse_port() {
        let url = Url::parse("http://localhost:8088").unwrap();
        assert_eq!(url.scheme(), Scheme::Http);
        assert_eq!(url.host(), "localhost");
        assert_eq!(url.port().unwrap(), 8088);
        assert_eq!(url.path(), "/");

        assert_eq!("http://localhost:8088/", std::format!("{}", url));
    }

    #[test]
    fn test_parse_port_path() {
        let url = Url::parse("http://localhost:8088/foo/bar").unwrap();
        assert_eq!(url.scheme(), Scheme::Http);
        assert_eq!(url.host(), "localhost");
        assert_eq!(url.port().unwrap(), 8088);
        assert_eq!(url.path(), "/foo/bar");

        assert_eq!("http://localhost:8088/foo/bar", std::format!("{}", url));
    }

    #[test]
    fn test_parse_scheme() {
        let url = Url::parse("https://localhost/").unwrap();
        assert_eq!(url.scheme(), Scheme::Https);
        assert_eq!(url.host(), "localhost");
        assert_eq!(url.port_or_default(), 443);
        assert_eq!(url.path(), "/");

        assert_eq!("https://localhost/", std::format!("{}", url));
    }

    #[test]
    fn test_parse_ipv4() {
        let url = Url::parse("https://127.0.0.1:1337/foo/bar").unwrap();
        assert_eq!(url.scheme(), Scheme::Https);
        assert_eq!(url.host(), "127.0.0.1");
        assert_eq!(
            url.host_socket_address().unwrap(),
            SocketAddr::from_str("127.0.0.1:1337").unwrap()
        );
        assert_eq!(url.port_or_default(), 1337);
        assert_eq!(url.path(), "/foo/bar");

        assert_eq!("https://127.0.0.1:1337/foo/bar", std::format!("{}", url));
    }

    #[test]
    fn test_parse_ipv6() {
        let url = Url::parse("https://[fe80::%1]/foo/bar").unwrap();
        assert_eq!(url.scheme(), Scheme::Https);
        assert_eq!(url.host(), "fe80::");
        assert_eq!(
            url.host_socket_address().unwrap(),
            SocketAddr::from_str("[fe80::%1]:443").unwrap()
        );
        assert_eq!(url.port_or_default(), 443);
        assert_eq!(url.path(), "/foo/bar");

        assert_eq!("https://[fe80::%1]/foo/bar", std::format!("{}", url));
    }

    #[test]
    fn test_parse_ipv6_port() {
        let url = Url::parse("https://[fe80::%1]:1337/foo/bar").unwrap();
        assert_eq!(url.scheme(), Scheme::Https);
        assert_eq!(url.host(), "fe80::");
        assert_eq!(
            url.host_socket_address().unwrap(),
            SocketAddr::from_str("[fe80::%1]:1337").unwrap()
        );
        assert_eq!(url.port_or_default(), 1337);
        assert_eq!(url.path(), "/foo/bar");

        assert_eq!("https://[fe80::%1]:1337/foo/bar", std::format!("{}", url));
    }

    #[test]
    fn test_invalid_ipv6() {
        assert_eq!(
            Url::parse("http://[fe80::/"),
            Err(ParseError::Ipv6AddressInvalid("[fe80::"))
        );
    }

    #[test]
    fn test_invalid_ipv6_port() {
        assert_eq!(
            Url::parse("http://[fe80]::::8080/"),
            Err(ParseError::InvalidPort(":::8080"))
        );
    }

    #[test]
    fn test_leftover_tokens_ipv6() {
        assert_eq!(
            Url::parse("http://[fe80]a/"),
            Err(ParseError::InvalidPort("a"))
        );
    }

    #[test]
    fn test_no_port_after_colon() {
        assert_eq!(
            Url::parse("http://localhost:/"),
            Err(ParseError::MissingPort)
        );
        assert_eq!(
            Url::parse("http://[fe80::]:/"),
            Err(ParseError::MissingPort)
        );
    }

    #[test]
    fn test_invalid_port() {
        assert_eq!(
            Url::parse("http://localhost:12E4/"),
            Err(ParseError::InvalidPort("12E4"))
        );
        assert_eq!(
            Url::parse("http://[fe80::]:12E4/"),
            Err(ParseError::InvalidPort("12E4"))
        );
    }
}
