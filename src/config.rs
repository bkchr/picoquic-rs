use super::VerifyCertificate;
use picoquic_sys::picoquic::PICOQUIC_RESET_SECRET_SIZE;

use std::time::Duration;
use std::path::PathBuf;

/// A role can either be `Server` or `Client`.
/// The role can be used to define which side is responsible for certain tasks, like sending
/// keep alive packages.
pub enum Role {
    Server,
    Client,
}

/// The file format of a certificate/private key.
pub enum FileFormat {
    PEM,
    DER,
}

/// Configuration used by `Context` to setup Picoquic.
pub struct Config {
    /// The path to the certificate chain(PEM format).
    pub cert_chain_filename: Option<PathBuf>,
    /// The certificate chain in memory in the given file format.
    pub cert_chain: Option<(FileFormat, Vec<Vec<u8>>)>,
    /// The path to the private key(PEM format).
    pub key_filename: Option<PathBuf>,
    /// The private key in memory in the given file format.
    pub key: Option<(FileFormat, Vec<u8>)>,
    /// The reset seed is used to create the stateless resets per `Connection`.
    pub reset_seed: Option<[u8; PICOQUIC_RESET_SECRET_SIZE as usize]>,
    /// The interval between keep alive packages. If the value is set to `Some(interval)`,
    /// each `Connection`, that matches the `keep_alive_sender` role, will send keep alive
    /// packages in the given `interval`.
    pub keep_alive_interval: Option<Duration>,
    /// The side of a `Connection` that is responsible for sending the keep alive packages.
    /// Default: `Role::Client`
    pub keep_alive_sender: Role,
    /// Sets TLS client authentication on the server.
    /// Default: false
    pub client_authentication: bool,
    /// The handler that should verify the peer certificate in the TLS handshake.
    pub verify_certificate_handler: Option<Box<VerifyCertificate>>,
}

impl Config {
    /// Creates a new `Config`.
    pub fn new() -> Config {
        Config {
            cert_chain_filename: None,
            cert_chain: None,
            key_filename: None,
            key: None,
            reset_seed: None,
            keep_alive_interval: None,
            keep_alive_sender: Role::Client,
            client_authentication: false,
            verify_certificate_handler: None,
        }
    }

    /// Sets the certificate(PEM format) chain filename.
    pub fn set_cert_chain_filename<C: Into<PathBuf>>(&mut self, path: C) {
        self.cert_chain_filename = Some(path.into())
    }

    /// Sets the private key(PEM format) filename.
    pub fn set_key_filename<P: Into<PathBuf>>(&mut self, path: P) {
        self.key_filename = Some(path.into())
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

    /// Enables TLS client authentication on the server.
    pub fn enable_client_authentication(&mut self) {
        self.client_authentication = true;
    }

    /// Sets the handler that should verify the peer certificate in the TLS handshake.
    pub fn set_verify_certificate_handler<H: VerifyCertificate + 'static>(&mut self, handler: H) {
        self.verify_certificate_handler = Some(Box::new(handler));
    }

    /// Sets the certificate chain.
    /// This option will overwrite `set_cert_chain_filename`.
    pub fn set_cert_chain(&mut self, certs: Vec<Vec<u8>>, format: FileFormat) {
        self.cert_chain = Some((format, certs));
    }

    /// Sets the private key.
    /// This option will overwrite `set_key_filename`.
    pub fn set_key(&mut self, key: Vec<u8>, format: FileFormat) {
        self.key = Some((format, key));
    }
}
