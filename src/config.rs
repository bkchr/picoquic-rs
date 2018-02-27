use picoquic_sys::picoquic::PICOQUIC_RESET_SECRET_SIZE;

use std::time::Duration;

/// A role can either be `Server` or `Client`.
/// The role can be used to define which side is responsible for certain tasks, like sending
/// keep alive packages.
pub enum Role {
    Server,
    Client,
}

/// Configuration used by `Context` to setup Picoquic.
pub struct Config {
    /// The path to the certificate.
    pub cert_filename: Option<String>,
    /// The path to the private key.
    pub key_filename: Option<String>,
    /// The reset seed is used to create the stateless resets per `Connection`.
    pub reset_seed: Option<[u8; PICOQUIC_RESET_SECRET_SIZE as usize]>,
    /// The interval between keep alive packages. If the value is set to `Some(interval)`,
    /// each `Connection`, that matches the `keep_alive_sender` role, will send keep alive
    /// packages in the given `interval`.
    pub keep_alive_interval: Option<Duration>,
    /// The side of a `Connection` that is responsible for sending the keep alive packages.
    /// Default: `Role::Client`
    pub keep_alive_sender: Role,
}

impl Config {
    /// Creates a `Config` that makes a `Context` usable as server. A server is also able to act as
    /// client. So, if you want to support P2P connections, you should use this `Config`.
    pub fn server(cert_filename: &str, key_filename: &str) -> Config {
        Config {
            cert_filename: Some(cert_filename.to_owned()),
            key_filename: Some(key_filename.to_owned()),
            reset_seed: None,
            keep_alive_interval: None,
            keep_alive_sender: Role::Client,
        }
    }

    /// Creates a `Config` that makes a `Context` usable as client.
    pub fn client() -> Config {
        Config {
            cert_filename: None,
            key_filename: None,
            reset_seed: None,
            keep_alive_interval: None,
            keep_alive_sender: Role::Client,
        }
    }

    /// Enables keep alive.
    pub fn enable_keep_alive(&mut self, dur: Duration) {
        self.keep_alive_interval = Some(dur);
    }

    /// Sets the sender for the keep alive messages.
    /// The default value is `Role::Client`. This value should be the same on the server and the
    /// client, otherwise both send continuously useless messages.
    pub fn set_keep_alive_sender(&mut self, role: Role) {
        self.keep_alive_sender = role;
    }
}
