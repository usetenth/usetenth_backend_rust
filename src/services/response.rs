use axum::Json;
use axum::http::StatusCode;
use serde::Serialize;

#[derive(Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub success: bool,
    pub message: String,
    pub data: Option<T>,
}

impl<T: Serialize> ApiResponse<T> {
    pub fn ok(message: &str, data: T) -> (StatusCode, Json<Self>) {
        (
            StatusCode::OK,
            Json(Self {
                success: true,
                message: message.into(),
                data: Some(data),
            }),
        )
    }

    pub fn internal_server_error() -> (StatusCode, Json<Self>) {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(Self {
                success: false,
                message: "Internal Server Error".into(),
                data: None,
            }),
        )
    }

    pub fn not_found() -> (StatusCode, Json<Self>) {
        (
            StatusCode::NOT_FOUND,
            Json(Self {
                success: false,
                message: "Resource not found".into(),
                data: None,
            }),
        )
    }

    pub fn created() -> (StatusCode, Json<Self>) {
        (
            StatusCode::CREATED,
            Json(Self {
                success: false,
                message: "Resource created".into(),
                data: None,
            }),
        )
    }

    pub fn unauthorized() -> (StatusCode, Json<Self>) {
        (
            StatusCode::UNAUTHORIZED,
            Json(Self {
                success: false,
                message: "Unauthorized".into(),
                data: None,
            }),
        )
    }
}
