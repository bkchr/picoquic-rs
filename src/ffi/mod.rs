use std::ops::Deref;

mod connection;
mod quic_ctx;
mod stateless_packet;
mod verify_certificate;

pub use self::connection::Connection;
pub use self::quic_ctx::MicroSeconds;
pub use self::quic_ctx::QuicCtx;

#[derive(Copy, Clone)]
pub struct Pointer<T>(*mut T);

impl<T> Deref for Pointer<T> {
    type Target = *mut T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

unsafe impl<T> Send for Pointer<T> {}
unsafe impl<T> Sync for Pointer<T> {}
