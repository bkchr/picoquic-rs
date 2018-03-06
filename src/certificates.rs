use openssl::x509::X509;
use openssl::pkey::{PKey, Public};
use openssl::error::ErrorStack;

pub type PubKey = PKey<Public>;

pub trait VerifyCertificate {
    fn verify(&mut self, cert: X509, chain: Vec<X509>) -> Result<PubKey, ErrorStack>;
}
