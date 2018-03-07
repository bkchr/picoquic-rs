use openssl::x509::X509;
use openssl::pkey::{PKey, Public};
use openssl::error::ErrorStack;
use openssl::stack::Stack;

pub type PubKey = PKey<Public>;

pub trait VerifyCertificate {
    fn verify(&mut self, cert: X509, chain: Stack<X509>) -> Result<PubKey, ErrorStack>;
}
