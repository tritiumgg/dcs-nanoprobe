//! The error model of spec/errors.md on the wire.
//!
//! A failed call's gRPC status carries a `google.rpc.Status` in its details,
//! as gRPC's richer error model does, and that holds one
//! [`ErrorDetail`](crate::v1alpha1::ErrorDetail) packed in an `Any`.

use prost::Message;

use crate::v1alpha1::{ErrorDetail, ErrorReason};

/// The `Any` type URL of [`ErrorDetail`].
pub const ERROR_DETAIL_TYPE_URL: &str = "type.googleapis.com/nanoprobe.v1alpha1.ErrorDetail";

/// `google.rpc.Status`, as the status details carry it.
#[derive(Clone, PartialEq, Message)]
struct RpcStatus {
    #[prost(int32, tag = "1")]
    code: i32,
    #[prost(string, tag = "2")]
    message: String,
    #[prost(message, repeated, tag = "3")]
    details: Vec<prost_types::Any>,
}

/// A status with the reason, message, entity and field of spec/errors.md.
pub fn status(
    code: tonic::Code,
    reason: ErrorReason,
    message: impl Into<String>,
    entity_id: u64,
    field: &str,
) -> tonic::Status {
    let message = message.into();
    let detail = ErrorDetail {
        reason: reason as i32,
        message: message.clone(),
        entity_id,
        field: field.to_owned(),
    };
    let rpc = RpcStatus {
        code: code as i32,
        message: message.clone(),
        details: vec![prost_types::Any {
            type_url: ERROR_DETAIL_TYPE_URL.to_owned(),
            value: detail.encode_to_vec(),
        }],
    };
    tonic::Status::with_details(code, message, rpc.encode_to_vec().into())
}

/// The [`ErrorDetail`] a status carries, if any.
pub fn detail(status: &tonic::Status) -> Option<ErrorDetail> {
    let rpc = RpcStatus::decode(status.details()).ok()?;
    rpc.details
        .iter()
        .find(|a| a.type_url == ERROR_DETAIL_TYPE_URL)
        .and_then(|a| ErrorDetail::decode(a.value.as_slice()).ok())
}

/// The reason a status carries, `Unspecified` for none.
pub fn reason(status: &tonic::Status) -> ErrorReason {
    detail(status)
        .and_then(|d| ErrorReason::try_from(d.reason).ok())
        .unwrap_or(ErrorReason::Unspecified)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_round_trips() {
        let s = status(
            tonic::Code::Aborted,
            ErrorReason::LeaseLost,
            "the lease epoch 3 is not the current one",
            7,
            "write.lease",
        );
        assert_eq!(s.code(), tonic::Code::Aborted);
        let d = detail(&s).expect("detail");
        assert_eq!(d.reason, ErrorReason::LeaseLost as i32);
        assert_eq!(d.entity_id, 7);
        assert_eq!(d.field, "write.lease");
        assert_eq!(reason(&s), ErrorReason::LeaseLost);
        assert_eq!(
            reason(&tonic::Status::internal("x")),
            ErrorReason::Unspecified
        );
    }
}
