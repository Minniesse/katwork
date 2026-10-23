use axum::{
    Json,
    http::{StatusCode, header::RETRY_AFTER},
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
            KatError::ResetTokenInvalid
            | KatError::InviteTokenInvalid
            | KatError::PasswordInvalid
            | KatError::RequestInvalid => StatusCode::BAD_REQUEST,
            KatError::UserExists | KatError::WorkspaceAmbiguous => StatusCode::CONFLICT,
            KatError::RateLimited { retry_after_secs } => {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    [(RETRY_AFTER, retry_after_secs.to_string())],
                    Json(serde_json::json!({"code": self.0.code()})),
                )
                    .into_response();
            }
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
        (status, Json(serde_json::json!({"code": self.0.code()}))).into_response()
    }
}
