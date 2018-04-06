use {ConnectionId, ConnectionType};

use openssl::error::ErrorStack;
use openssl::stack::StackRef;
use openssl::x509::store::X509StoreRef;
use openssl::x509::{X509, X509Ref, X509StoreContext};

/// The `VerifyCertificate` trait is used by the verify certificate handler, to verify a
/// certificate.
pub trait VerifyCertificate {
    /// Will be called to verify the given certificate and certificates chain.
    ///
    /// # Result
    ///
    /// If the certificate could be verified, the function should return `Ok(())`, otherwise
    /// a `Err(ErrorStack)` is expected.
    fn verify(
        &mut self,
        connection_id: ConnectionId,
        connection_type: ConnectionType,
        cert: &X509Ref,
        chain: &StackRef<X509>,
    ) -> Result<bool, ErrorStack>;
}

/// Provides a default implementation for verifying a certificate and certificates chain against
/// a `X509Store` with trusted certificates.
pub fn default_verify_certificate(
    cert: &X509Ref,
    chain: &StackRef<X509>,
    store: &X509StoreRef,
) -> Result<bool, ErrorStack> {
    let mut context = X509StoreContext::new()?;
    context.init(store, cert, chain, |c| c.verify_cert())
}
