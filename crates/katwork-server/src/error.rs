use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use katwork::error::KatError;

pub struct ApiError(pub KatError);

impl From<KatError> for ApiError {
    fn from(error: KatError) -> Self {
        Self(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            KatError::JobAlreadyFinished | KatError::JobNotAwaitingInput => StatusCode::CONFLICT,
            KatError::InvalidCursor => StatusCode::BAD_REQUEST,
            KatError::TooManyStreams => StatusCode::INTERNAL_SERVER_ERROR,
            KatError::Busy => StatusCode::SERVICE_UNAVAILABLE,
            KatError::MessageTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            KatError::ClientMsgIdConflict => StatusCode::CONFLICT,
            KatError::MessageEmpty
            | KatError::MessageInvalid
            | KatError::ClientMsgIdInvalid
            | KatError::ClientMsgIdTooLong
            | KatError::ThreadTitleInvalid => StatusCode::UNPROCESSABLE_ENTITY,
            KatError::NotFound => StatusCode::NOT_FOUND,
            KatError::Forbidden => StatusCode::FORBIDDEN,
            KatError::KatNameConflict
            | KatError::DefaultSpaceRequired
            | KatError::GrantInUse
            | KatError::TeamLeadConflict
            | KatError::TeamLeadRequired
            | KatError::KatNotInSpace
            | KatError::SpacePolicyRequired => StatusCode::UNPROCESSABLE_ENTITY,
            KatError::Unauthenticated => StatusCode::UNAUTHORIZED,
            KatError::ResetTokenInvalid | KatError::PasswordInvalid | KatError::RequestInvalid => {
                StatusCode::BAD_REQUEST
            }
            KatError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            KatError::BodyTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            KatError::AuthFailed | KatError::MailDeliveryFailed => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            KatError::HostNotAllowed | KatError::CrossSiteRequest => StatusCode::FORBIDDEN,
            KatError::JsonRequired => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            KatError::AuthBusy | KatError::DatabaseUnavailable(_) => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            KatError::LeaseLost => StatusCode::CONFLICT,
            KatError::ConfigInvalid { .. }
            | KatError::CommandInvalid
            | KatError::Database(_)
            | KatError::Migration(_)
            | KatError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let mut response =
            (status, Json(serde_json::json!({"code": self.0.code()}))).into_response();
        if matches!(self.0, KatError::Busy) {
            response
                .headers_mut()
                .insert("retry-after", axum::http::HeaderValue::from_static("1"));
        }
        response
    }
}
