//! NIN lookups. Prembly and Dojah are normalised to one shape, and only an upstream failure moves on to the next vendor.

use serde_json::Value;
use std::time::Duration;

#[derive(Clone)]
pub enum VendorConfig {
    Prembly { base_url: String, api_key: String, nin_path: Option<String> },
    Dojah { base_url: String, app_id: String, secret_key: String, nin_path: Option<String> },
}

impl VendorConfig {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Prembly { .. } => "prembly",
            Self::Dojah { .. } => "dojah",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NinEntity {
    pub first_name: String,
    pub middle_name: String,
    pub last_name: String,
    pub vendor: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Upstream,
    NotFound,
    InvalidInput,
}

#[derive(Debug, Clone)]
pub struct VendorError {
    pub kind: ErrorKind,
    pub vendor: String,
    pub message: String,
}

fn err(kind: ErrorKind, vendor: &str, message: impl Into<String>) -> VendorError {
    VendorError { kind, vendor: vendor.into(), message: message.into() }
}

fn text(value: &Value, keys: &[&str]) -> String {
    keys.iter().find_map(|k| value.get(*k).and_then(Value::as_str)).unwrap_or("").trim().to_string()
}

fn entity(vendor: &str, first: String, middle: String, last: String) -> Result<NinEntity, VendorError> {
    if first.is_empty() || last.is_empty() {
        return Err(err(ErrorKind::Upstream, vendor, "record is missing name fields"));
    }
    Ok(NinEntity { first_name: first, middle_name: middle, last_name: last, vendor: vendor.into() })
}

/// Prembly's docs list no error bodies, so failures are told apart by status and message.
pub fn classify_prembly(status: Option<u16>, body: &Value) -> VendorError {
    let message = ["detail", "message"].iter().find_map(|k| body.get(*k).and_then(Value::as_str)).unwrap_or("").to_lowercase();
    let account_problem = ["insufficient", "wallet", "balance", "unauthoris", "unauthoriz", "authentication", "credentials", "api key", "api-key", "apikey"]
        .iter()
        .any(|w| message.contains(w))
        || body["errors"]["code"] == "authentication_failed";
    if account_problem || matches!(status, Some(401 | 402 | 403)) {
        return err(ErrorKind::Upstream, "prembly", "account problem");
    }
    if status == Some(404) || ["not found", "no record", "does not exist", "doesn't exist"].iter().any(|w| message.contains(w)) {
        return err(ErrorKind::NotFound, "prembly", "NIN not found");
    }
    if matches!(status, Some(400 | 422)) {
        return err(ErrorKind::InvalidInput, "prembly", "NIN rejected by vendor");
    }
    err(ErrorKind::Upstream, "prembly", "request failed")
}

pub fn parse_prembly(body: &Value) -> Result<NinEntity, VendorError> {
    let data = body.get("data").or_else(|| body.get("nin_data"));
    let Some(data) = data.filter(|d| d.is_object()).filter(|_| body["status"] == true) else {
        return Err(classify_prembly(None, body));
    };
    if data["nin_suspension_status"] == true {
        return Err(err(ErrorKind::NotFound, "prembly", "NIN is suspended"));
    }
    entity("prembly", text(data, &["firstname"]), text(data, &["middlename"]), text(data, &["surname"]))
}

pub fn parse_dojah(body: &Value) -> Result<NinEntity, VendorError> {
    let Some(data) = body.get("entity").filter(|e| e.is_object()) else {
        return Err(err(ErrorKind::Upstream, "dojah", "returned no entity"));
    };
    entity(
        "dojah",
        text(data, &["first_name", "firstname"]),
        text(data, &["middle_name", "middlename"]),
        text(data, &["last_name", "surname", "lastname"]),
    )
}

async fn prembly(http: &reqwest::Client, base_url: &str, api_key: &str, nin_path: &Option<String>, nin: &str) -> Result<NinEntity, VendorError> {
    let url = format!("{}{}", base_url.trim_end_matches('/'), nin_path.as_deref().unwrap_or("/verification/vnin-basic"));
    let response = http
        .post(url)
        .header("x-api-key", api_key)
        .json(&serde_json::json!({ "number": nin }))
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|_| err(ErrorKind::Upstream, "prembly", "request failed"))?;
    let status = response.status().as_u16();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !(200..300).contains(&status) {
        return Err(classify_prembly(Some(status), &body));
    }
    parse_prembly(&body)
}

async fn dojah(http: &reqwest::Client, base_url: &str, app_id: &str, secret: &str, nin_path: &Option<String>, nin: &str) -> Result<NinEntity, VendorError> {
    let url = format!("{}{}", base_url.trim_end_matches('/'), nin_path.as_deref().unwrap_or("/api/v1/kyc/nin"));
    let response = http
        .get(url)
        .header("AppId", app_id)
        .header("Authorization", secret)
        .query(&[("nin", nin)])
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|_| err(ErrorKind::Upstream, "dojah", "request failed"))?;
    match response.status().as_u16() {
        200..=299 => parse_dojah(&response.json().await.unwrap_or(Value::Null)),
        404 => Err(err(ErrorKind::NotFound, "dojah", "NIN not found")),
        400 | 422 => Err(err(ErrorKind::InvalidInput, "dojah", "NIN rejected by vendor")),
        // Timeouts, auth or wallet problems, rate limits and 5xx all fail over to the next vendor.
        _ => Err(err(ErrorKind::Upstream, "dojah", "request failed")),
    }
}

pub async fn lookup(http: &reqwest::Client, vendor: &VendorConfig, nin: &str) -> Result<NinEntity, VendorError> {
    match vendor {
        VendorConfig::Prembly { base_url, api_key, nin_path } => prembly(http, base_url, api_key, nin_path, nin).await,
        VendorConfig::Dojah { base_url, app_id, secret_key, nin_path } => dojah(http, base_url, app_id, secret_key, nin_path, nin).await,
    }
}

/// Tries vendors in order; a definitive answer (found, not found, invalid) stops the search.
pub async fn lookup_with_failover(http: &reqwest::Client, vendors: &[VendorConfig], nin: &str) -> Result<NinEntity, VendorError> {
    let mut last = err(ErrorKind::Upstream, "none", "no vendor is configured");
    for vendor in vendors {
        match lookup(http, vendor, nin).await {
            Err(e) if e.kind == ErrorKind::Upstream => {
                eprintln!("KYC vendor {} failed, trying the next: {}", e.vendor, e.message);
                last = e;
            }
            other => return other,
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prembly_records_are_normalised() {
        let body = json!({ "status": true, "data": { "firstname": " Adaeze ", "middlename": "", "surname": "Obi", "nin_suspension_status": false } });
        assert_eq!(parse_prembly(&body).unwrap(), NinEntity { first_name: "Adaeze".into(), middle_name: "".into(), last_name: "Obi".into(), vendor: "prembly".into() });
    }

    #[test]
    fn prembly_failures_are_classified() {
        let kind = |status, body: Value| classify_prembly(status, &body).kind;
        assert_eq!(kind(Some(404), json!({})), ErrorKind::NotFound);
        assert_eq!(kind(Some(200), json!({ "detail": "No record found" })), ErrorKind::NotFound);
        assert_eq!(kind(Some(422), json!({})), ErrorKind::InvalidInput);
        assert_eq!(kind(Some(402), json!({})), ErrorKind::Upstream, "an account problem is ours, not the user's");
        assert_eq!(kind(Some(400), json!({ "detail": "Insufficient wallet balance" })), ErrorKind::Upstream);
        assert_eq!(kind(Some(500), json!({})), ErrorKind::Upstream);
        assert_eq!(parse_prembly(&json!({ "status": true, "data": { "firstname": "A", "surname": "B", "nin_suspension_status": true } })).unwrap_err().kind, ErrorKind::NotFound);
        assert_eq!(parse_prembly(&json!({ "status": false, "data": {} })).unwrap_err().kind, ErrorKind::Upstream);
    }

    #[test]
    fn dojah_records_are_normalised() {
        let body = json!({ "entity": { "first_name": "Adaeze", "surname": "Obi" } });
        assert_eq!(parse_dojah(&body).unwrap().last_name, "Obi");
        assert_eq!(parse_dojah(&json!({})).unwrap_err().kind, ErrorKind::Upstream);
        assert_eq!(parse_dojah(&json!({ "entity": { "first_name": "Adaeze" } })).unwrap_err().kind, ErrorKind::Upstream);
    }
}
