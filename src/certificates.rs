use openssl::x509::X509;
use openssl::error::ErrorStack;
use openssl::stack::Stack;

pub trait VerifyCertificate {
    fn verify(&mut self, cert: &X509, chain: &Stack<X509>) -> Result<(), ErrorStack>;
}
