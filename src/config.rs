use super::VerifyCertificate;
use picoquic_sys::picoquic::PICOQUIC_RESET_SECRET_SIZE;

use std::path::PathBuf;
use std::time::Duration;

/// A role can either be `Server` or `Client`.
/// The role can be used to define which side is responsible for certain tasks, like sending
/// keep alive packages.
#[derive(Clone, Copy, PartialEq)]
pub enum Role {
    Server,
    Client,
}

/// The file format of a certificate/private private_key.
#[derive(Clone, Copy, PartialEq)]
pub enum FileFormat {
    PEM,
    DER,
}

/// Configuration used by `Context` to setup Picoquic.
pub struct Config {
    /// The path to the certificate chain(PEM format).
    pub certificate_chain_filename: Option<PathBuf>,
    /// The certificate chain in memory in the given file format.
    pub certificate_chain: Option<(FileFormat, Vec<Vec<u8>>)>,
    /// The path to the root certificate (PEM format).
    pub root_certificate_filename: Option<PathBuf>,
    /// The root certificate in memory in the given file format.
    pub root_certificates: Option<(FileFormat, Vec<Vec<u8>>)>,
    /// The path to the private private_key(PEM format).
    pub private_key_filename: Option<PathBuf>,
    /// The private private_key in memory in the given file format.
    pub private_key: Option<(FileFormat, Vec<u8>)>,
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
            certificate_chain_filename: None,
            certificate_chain: None,
            root_certificate_filename: None,
            root_certificates: None,
            private_key_filename: None,
            private_key: None,
            reset_seed: None,
            keep_alive_interval: None,
            keep_alive_sender: Role::Client,
            client_authentication: false,
            verify_certificate_handler: None,
        }
    }

    /// Will create a new instance by cloning another `Config`.
    /// The `verify_certificate_handler` will be set to `None` as it does not support to be cloned.
    pub fn clone_from(other: &Config) -> Config {
        Config {
            certificate_chain_filename: other.certificate_chain_filename.clone(),
            certificate_chain: other.certificate_chain.clone(),
            root_certificates: other.root_certificates.clone(),
            root_certificate_filename: other.root_certificate_filename.clone(),
            private_key_filename: other.private_key_filename.clone(),
            private_key: other.private_key.clone(),
            reset_seed: other.reset_seed.clone(),
            keep_alive_interval: other.keep_alive_interval.clone(),
            keep_alive_sender: other.keep_alive_sender,
            client_authentication: other.client_authentication,
            verify_certificate_handler: None,
        }
    }

    /// Sets the certificate chain(PEM format) filename.
    pub fn set_certificate_chain_filename<C: Into<PathBuf>>(&mut self, path: C) {
        self.certificate_chain_filename = Some(path.into())
    }

    /// Sets the private key(PEM format) filename.
    pub fn set_private_key_filename<P: Into<PathBuf>>(&mut self, path: P) {
        self.private_key_filename = Some(path.into())
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

    /// Sets the certificate.
    /// This option will overwrite `set_certificate_chain_filename`.
    pub fn set_certificate_chain(&mut self, certs: Vec<Vec<u8>>, format: FileFormat) {
        self.certificate_chain = Some((format, certs));
    }

    /// Sets the private private_key.
    /// This option will overwrite `set_private_key_filename`.
    pub fn set_private_key(&mut self, private_key: Vec<u8>, format: FileFormat) {
        self.private_key = Some((format, private_key));
    }

    /// Sets the root certificate(PEM format) filename.
    pub fn set_root_certificate_filename<P: Into<PathBuf>>(&mut self, path: P) {
        self.root_certificate_filename = Some(path.into())
    }

    /// Sets the root certificates.
    /// This option will overwrite `set_root_certificate_filename`.
    pub fn set_root_certificates(&mut self, certificates: Vec<Vec<u8>>, format: FileFormat) {
        self.root_certificates = Some((format, certificates));
    }
}
