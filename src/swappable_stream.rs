use futures::{Async::Ready, Poll, Stream};

use std::{fmt, ops::{Deref, DerefMut}};

pub struct SwappableStream<S> {
    active_stream: S,
    swap_to: Option<S>,
}

impl<S> fmt::Debug for SwappableStream<S> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "SwappableStream")
    }
}

impl<S> SwappableStream<S> {
    pub fn new(active_stream: S) -> Self {
        Self {
            active_stream,
            swap_to: None,
        }
    }

    pub fn set_swap_to(&mut self, swap_to: S) {
        self.swap_to = Some(swap_to);
    }
}

impl<S: Stream> Stream for SwappableStream<S> {
    type Item = <S as Stream>::Item;
    type Error = <S as Stream>::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        loop {
            match try_ready!(self.active_stream.poll()) {
                Some(val) => return Ok(Ready(Some(val))),
                None => match self.swap_to.take() {
                    Some(new_stream) => self.active_stream = new_stream,
                    None => return Ok(Ready(None)),
                },
            }
        }
    }
}

impl<S> From<S> for SwappableStream<S> {
    fn from(stream: S) -> Self {
        SwappableStream::new(stream)
    }
}

impl<S> Deref for SwappableStream<S> {
    type Target = S;

    fn deref(&self) -> &Self::Target {
        &self.active_stream
    }
}

impl<S> DerefMut for SwappableStream<S> {
    fn deref_mut(&mut self) -> &mut S {
        &mut self.active_stream
    }
}
