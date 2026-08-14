//! Cloud API proxy — keeps the API key server-side (never shipped to frontend).
//!
//! The frontend calls these Tauri commands instead of directly hitting the cloud API.
//! This prevents the API key from being exposed in the client bundle.

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::settings::SettingsStore;

/// Pooled HTTP client — reuses connections and TLS sessions across requests.
pub struct HttpClient(pub reqwest::Client);

impl HttpClient {
    pub fn new() -> Self {
        Self(
            reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        )
    }
}

/// The cloud API base URL and key. The key stays here in the backend binary,
/// which is much harder to extract than a plain JS string in a webview bundle.
pub(crate) const CLOUD_API_BASE: &str = "https://clipsta-api.godson594.workers.dev";
pub(crate) const CLOUD_API_KEY: &str = "32b28eac803a1b24c19e20665919eaeb7f1493d2b5e3f68be7944db6d9f01b96";

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingResponse {
    pub token: String,
    pub pairing_url: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipUploadRequest {
    pub desktop_device_id: String,
    pub file_name: String,
    pub duration_seconds: u32,
    pub bytes: u64,
    pub captured_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipUploadResponse {
    pub upload_url: String,
    pub stream_uid: Option<String>,
    pub share_url: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadStatusBody {
    pub desktop_device_id: String,
    pub desktop_name: String,
    pub queued_count: u32,
    pub waiting_for_gameplay_count: u32,
    pub uploading_count: u32,
    pub uploaded_count: u32,
    pub failed_count: u32,
    pub current_progress_percent: u32,
    pub current_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudConfigPublic {
    /// Only the base URL is exposed to the frontend (no key).
    pub api_base: String,
}

// ── Commands ──────────────────────────────────────────────────────────────────

/// Returns the cloud config (base URL only — NO API KEY).
#[tauri::command]
pub async fn cloud_get_config() -> Result<CloudConfigPublic, String> {
    Ok(CloudConfigPublic {
        api_base: CLOUD_API_BASE.to_string(),
    })
}

/// Generate a pairing token via the cloud API (API key stays backend-side).
#[tauri::command]
pub async fn cloud_generate_pairing(
    store: State<'_, SettingsStore>,
    http: State<'_, HttpClient>,
) -> Result<PairingResponse, String> {
    let settings = store.get();
    let device_id = if settings.desktop_device_id.is_empty() {
        format!("desktop_{}", uuid::Uuid::new_v4().simple())
    } else {
        settings.desktop_device_id.clone()
    };

    let res = http.0
        .post(format!("{}/pairing-tokens", CLOUD_API_BASE))
        .header("Content-Type", "application/json")
        .header("X-Clipsta-Test-Key", CLOUD_API_KEY)
        .json(&serde_json::json!({
            "desktopDeviceId": device_id,
            "desktopName": "Clipsta Desktop",
        }))
        .send()
        .await
        .map_err(|e| format!("Pairing request failed: {}", e))?;

    if !res.status().is_success() {
        let status = res.status().as_u16();
        let body = res.text().await.unwrap_or_default();
        return Err(format!("Pairing failed: HTTP {} {}", status, body));
    }

    let data: PairingResponse = res
        .json()
        .await
        .map_err(|e| format!("Failed to parse pairing response: {}", e))?;

    // Store the pairing code
    store.set_field("cloudPairCode", serde_json::Value::String(data.token.clone()));

    Ok(data)
}

/// Request an upload URL from the cloud API (API key stays backend-side).
#[tauri::command]
pub async fn cloud_request_upload(
    http: State<'_, HttpClient>,
    req: ClipUploadRequest,
) -> Result<ClipUploadResponse, String> {
    let res = http.0
        .post(format!("{}/clip-uploads", CLOUD_API_BASE))
        .header("Content-Type", "application/json")
        .header("X-Clipsta-Test-Key", CLOUD_API_KEY)
        .json(&req)
        .send()
        .await
        .map_err(|e| format!("Upload request failed: {}", e))?;

    if !res.status().is_success() {
        let status = res.status().as_u16();
        let body = res.text().await.unwrap_or_default();
        return Err(format!("clip-uploads failed: HTTP {} {}", status, body));
    }

    let data: ClipUploadResponse = res
        .json()
        .await
        .map_err(|e| format!("Failed to parse upload response: {}", e))?;

    Ok(data)
}

/// Notify the cloud API of upload status (API key stays backend-side).
#[tauri::command]
pub async fn cloud_notify_status(
    http: State<'_, HttpClient>,
    body: UploadStatusBody,
) -> Result<(), String> {
    let _ = http.0
        .post(format!("{}/desktop-upload-status", CLOUD_API_BASE))
        .header("Content-Type", "application/json")
        .header("X-Clipsta-Test-Key", CLOUD_API_KEY)
        .json(&body)
        .send()
        .await;
    // Fire-and-forget — don't fail if status notification fails
    Ok(())
}
