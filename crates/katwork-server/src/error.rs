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
            KatError::DatabaseUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            KatError::LeaseLost => StatusCode::CONFLICT,
            KatError::ConfigInvalid
            | KatError::CommandInvalid
            | KatError::Database(_)
            | KatError::Migration(_)
            | KatError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(serde_json::json!({"code": self.0.code()}))).into_response()
    }
}
