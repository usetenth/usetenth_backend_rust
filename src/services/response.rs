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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_wraps_the_data_in_the_standard_envelope() {
        let (status, body) = ApiResponse::ok("Done", vec![1, 2]);
        assert_eq!(status, StatusCode::OK);
        let json = serde_json::to_value(&body.0).unwrap();
        assert_eq!(json, serde_json::json!({ "success": true, "message": "Done", "data": [1, 2] }));
    }
}
