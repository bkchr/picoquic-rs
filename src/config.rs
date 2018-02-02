use picoquic_sys::picoquic::PICOQUIC_RESET_SECRET_SIZE;

/// Configuration used by `Context` to setup Picoquic.
pub struct Config {
    /// The path to the certificate.
    pub cert_filename: Option<String>,
    /// The path to the private key.
    pub key_filename: Option<String>,
    /// The reset seed is used to create the stateless resets per `Connection`.
    pub reset_seed: Option<[u8; PICOQUIC_RESET_SECRET_SIZE as usize]>,
}

impl Config {
    /// Creates a `Config` that makes a `Context` usable as server. A server is also able to act as
    /// client. So, if you want to support P2P connections, you should use this `Config`.
    pub fn server(cert_filename: &str, key_filename: &str) -> Config {
        Config {
            cert_filename: Some(cert_filename.to_owned()),
            key_filename: Some(key_filename.to_owned()),
            reset_seed: None,
        }
    }

    /// Creates a `Config` that makes a `Context` usable as client.
    pub fn client() -> Config {
        Config {
            cert_filename: None,
            key_filename: None,
            reset_seed: None,
        }
    }
}
