use crate::error::*;

use futures::{sync::mpsc, Poll, Sink, StartSend, Stream};

use std::{fmt, sync::Arc};

use parking_lot::RwLock;

/// A state for storing a shared error.
#[derive(Clone)]
struct ErrorState {
    inner: Arc<RwLock<Option<Box<dyn ErrorFn<Output = Error>>>>>,
}

impl fmt::Debug for ErrorState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ErrorState")
    }
}

impl ErrorState {
    fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
        }
    }
    /// Return the stored error.
    pub fn error(&self) -> Option<Error> {
        self.inner.read().as_ref().map(|v| v())
    }

    /// Set the stored error function.
    pub fn set_error<T: ErrorFn>(&mut self, err_fn: T) {
        *self.inner.write() = Some(Box::new(err_fn));
    }
}

/// A trait that can be used to check if a channel is closed.
pub trait IsClosed {
    fn is_closed(&self) -> bool;
}

impl<T> IsClosed for mpsc::Sender<T> {
    fn is_closed(&self) -> bool {
        mpsc::Sender::is_closed(self)
    }
}

impl<T> IsClosed for mpsc::UnboundedSender<T> {
    fn is_closed(&self) -> bool {
        mpsc::UnboundedSender::is_closed(self)
    }
}

/// Error used by `Sender` to distinguish between an error set for the channel and
/// a normal `futures::SendError`.
pub enum SendError<T> {
    Channel(Error, T),
    Normal(T),
}

/// A sender with an associated error that can be set by the receiver side.
#[derive(Debug)]
pub struct SenderWithError<S> {
    sender: S,
    error_state: ErrorState,
}

impl<S: Clone> Clone for SenderWithError<S> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            error_state: self.error_state.clone(),
        }
    }
}

impl<S: IsClosed> SenderWithError<S> {
    fn new(sender: S, error_state: ErrorState) -> Self {
        Self {
            sender,
            error_state,
        }
    }

    fn check_for_error(&mut self) -> Option<Error> {
        // If `sender` is closed, check if we got an error to propagate
        if self.sender.is_closed() {
            self.error_state.error()
        } else {
            None
        }
    }

    fn map_err<E>(&mut self, e: E) -> SendError<E> {
        if let Some(err) = self.check_for_error() {
            SendError::Channel(err, e)
        } else {
            SendError::Normal(e)
        }
    }
}

impl<S: Sink + IsClosed> Sink for SenderWithError<S> {
    type SinkItem = <S as Sink>::SinkItem;
    type SinkError = SendError<<S as Sink>::SinkError>;

    fn start_send(&mut self, item: Self::SinkItem) -> StartSend<Self::SinkItem, Self::SinkError> {
        self.sender.start_send(item).map_err(|e| self.map_err(e))
    }

    fn poll_complete(&mut self) -> Poll<(), Self::SinkError> {
        self.sender.poll_complete().map_err(|e| self.map_err(e))
    }
}

/// A channel that can be closed.
pub trait Close {
    fn close(&mut self);
}

impl<T> Close for mpsc::Receiver<T> {
    fn close(&mut self) {
        mpsc::Receiver::close(self)
    }
}

impl<T> Close for mpsc::UnboundedReceiver<T> {
    fn close(&mut self) {
        mpsc::UnboundedReceiver::close(self)
    }
}

/// A receiver with the ability to propagate an error to the sender.
pub struct ReceiverWithError<R> {
    receiver: R,
    error_state: ErrorState,
}

impl<R: Close> ReceiverWithError<R> {
    fn new(receiver: R, error_state: ErrorState) -> Self {
        Self {
            receiver,
            error_state,
        }
    }

    /// Channel the given error to the `Sender` side.
    /// This will only work once, further calls of this function will just discard the error.
    pub fn propagate_error<F: ErrorFn + 'static>(&mut self, err: F) {
        self.error_state.set_error(err)
    }

    pub fn close(&mut self) {
        self.receiver.close();
    }
}

impl<R: Stream> Stream for ReceiverWithError<R> {
    type Item = <R as Stream>::Item;
    type Error = Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        self.receiver.poll().map_err(|_| ErrorKind::Unknown.into())
    }
}

pub type UnboundedSender<T> = SenderWithError<mpsc::UnboundedSender<T>>;
pub type UnboundedReceiver<T> = ReceiverWithError<mpsc::UnboundedReceiver<T>>;

pub fn unbounded<T>() -> (UnboundedSender<T>, UnboundedReceiver<T>) {
    let (sender, receiver) = mpsc::unbounded();
    let error_state = ErrorState::new();

    (
        SenderWithError::new(sender, error_state.clone()),
        ReceiverWithError::new(receiver, error_state),
    )
}

pub type Sender<T> = SenderWithError<mpsc::Sender<T>>;
pub type Receiver<T> = ReceiverWithError<mpsc::Receiver<T>>;

pub fn channel<T>(buffer: usize) -> (Sender<T>, Receiver<T>) {
    let (sender, receiver) = mpsc::channel(buffer);
    let error_state = ErrorState::new();

    (
        SenderWithError::new(sender, error_state.clone()),
        ReceiverWithError::new(receiver, error_state),
    )
}
