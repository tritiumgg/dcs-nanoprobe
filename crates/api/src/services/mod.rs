//! The gRPC services of plan 4.5. Each file is one service.

pub mod campaign;
pub mod commands;
pub mod events;
pub mod extension;
pub mod mission;
pub mod moderation;
pub mod players;
pub mod session;
pub mod world;

use std::pin::Pin;

use nanoprobe_proto::errors::status;
use nanoprobe_proto::v1alpha1::ErrorReason;
use tokio::sync::mpsc;
use tokio_stream::Stream;
use tonic::{Code, Status};

/// A server stream.
pub type Out<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send + 'static>>;

/// `CONSUMER_TOO_SLOW`.
pub fn too_slow() -> Status {
    status(
        Code::ResourceExhausted,
        ErrorReason::ConsumerTooSlow,
        "the consumer fell behind; list or subscribe again from the last revision or sequence number",
        0,
        "",
    )
}

/// Queue one message on a bounded stream. When only one slot is left, the
/// stream is ended with `CONSUMER_TOO_SLOW` in it instead, so a consumer
/// that stops reading costs at most the buffer. Returns whether the stream
/// goes on.
pub fn push<T>(tx: &mpsc::Sender<Result<T, Status>>, item: T) -> bool {
    if tx.is_closed() {
        return false;
    }
    if tx.capacity() <= 1 {
        let _ = tx.try_send(Err(too_slow()));
        return false;
    }
    tx.try_send(Ok(item)).is_ok()
}

/// The error that ends every stream when the server stops.
pub fn stopping() -> Status {
    status(
        Code::Unavailable,
        ErrorReason::Unspecified,
        "the server is stopping; connect again and resume from the last revision or sequence number",
        0,
        "",
    )
}

/// End a stream with an error.
pub fn end<T>(tx: &mpsc::Sender<Result<T, Status>>, e: Status) {
    let _ = tx.try_send(Err(e));
}
