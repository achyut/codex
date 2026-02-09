//! OAuth token refresh support for providers secured by expiring tokens.
//!
//! When a provider's `[oauth]` config section is present, an [`OAuthTokenManager`]
//! is created that:
//!   1. Performs an initial token refresh at startup.
//!   2. Spawns a background task that proactively refreshes before expiry.
//!   3. Exposes `current_api_token()` for cheap, non-blocking reads.

use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::RwLock;
use tracing::error;
use tracing::info;
use tracing::warn;

/// Default fallback refresh interval (25 minutes) if the server doesn't
/// return a usable `expiresIn` value.
const DEFAULT_REFRESH_INTERVAL_SECS: u64 = 1500;

/// Fraction of `expires_in` at which we proactively refresh (80%).
const REFRESH_BUFFER_FRACTION: f64 = 0.80;

/// Minimum sleep between refresh attempts to avoid tight loops on errors.
const MIN_RETRY_DELAY_SECS: u64 = 30;

// ---------------------------------------------------------------------------
// Config (deserialized from config.toml)
// ---------------------------------------------------------------------------

/// OAuth token refresh configuration for a model provider.
///
/// When present on a `ModelProviderInfo`, Codex will automatically refresh the
/// `api-token` header on a background timer using the specified refresh endpoint.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
pub struct OAuthConfig {
    /// Path to the refresh endpoint, relative to the provider's `base_url`.
    /// Example: `"tokens/refresh"`
    pub refresh_path: String,

    /// Fallback refresh interval in seconds. Used when the server does not
    /// return a usable `expiresIn` value. Defaults to 1500 (25 minutes).
    pub refresh_interval_secs: Option<u64>,

    /// Value for the `api-version` header sent on both refresh and API requests.
    pub api_version: String,

    /// Environment variable name that holds the `subscription-key` header value.
    pub subscription_key_env: String,

    /// Environment variable name that holds the initial refresh token
    /// (used for the very first refresh call).
    pub initial_refresh_token_env: String,

    /// Environment variable name that holds the initial `api-token` value
    /// (used for the very first refresh call). If absent, the first refresh
    /// sends an empty string.
    pub initial_api_token_env: Option<String>,
}

// ---------------------------------------------------------------------------
// Refresh endpoint response
// ---------------------------------------------------------------------------

/// JSON response returned by the OAuth token refresh endpoint.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OAuthRefreshResponse {
    /// The new bearer / api-token to use for API requests.
    api_token: String,
    /// New refresh token to use for the next refresh cycle.
    refresh_token: String,
    /// Token time-to-live in seconds (e.g. 3600 for 60 min).
    expires_in: u64,
    /// Whether the server actually issued a new token.
    has_refreshed: bool,
}

// ---------------------------------------------------------------------------
// In-memory token state
// ---------------------------------------------------------------------------

/// Shared mutable token state protected by an async `RwLock`.
struct OAuthTokenState {
    api_token: String,
    refresh_token: String,
    expires_in: u64,
}

// ---------------------------------------------------------------------------
// OAuthTokenManager
// ---------------------------------------------------------------------------

/// Manages the OAuth token lifecycle for a single provider.
///
/// Created once per Codex session (lazily on first use) and lives until the
/// session ends. A background `tokio::spawn` task proactively refreshes the
/// token before it expires.
pub struct OAuthTokenManager {
    state: Arc<RwLock<OAuthTokenState>>,
    /// Handle to the background refresh task so we can abort it on drop.
    _refresh_handle: tokio::task::JoinHandle<()>,
}

impl OAuthTokenManager {
    /// Create a new manager, perform the initial token refresh, and spawn the
    /// background refresh loop.
    ///
    /// # Errors
    /// Returns an error if required environment variables are missing or if
    /// the initial token refresh call fails.
    pub async fn new(config: &OAuthConfig, base_url: &str) -> anyhow::Result<Self> {
        let subscription_key = read_env_var(&config.subscription_key_env)?;
        let initial_refresh_token = read_env_var(&config.initial_refresh_token_env)?;
        let initial_api_token = match &config.initial_api_token_env {
            Some(env_name) => read_env_var(env_name).unwrap_or_default(),
            None => String::new(),
        };

        let refresh_url = build_refresh_url(base_url, &config.refresh_path);
        let api_version = config.api_version.clone();
        let fallback_interval = config
            .refresh_interval_secs
            .unwrap_or(DEFAULT_REFRESH_INTERVAL_SECS);

        let client = Client::builder().timeout(Duration::from_secs(30)).build()?;

        // Perform the initial refresh to get a valid token.
        let initial_state = refresh_once(
            &client,
            &refresh_url,
            &subscription_key,
            &api_version,
            &initial_api_token,
            &initial_refresh_token,
        )
        .await?;

        let state = Arc::new(RwLock::new(initial_state));

        // Spawn background refresh loop.
        let refresh_handle = {
            let state = Arc::clone(&state);
            let subscription_key = subscription_key.clone();
            let api_version = api_version.clone();
            let refresh_url = refresh_url.clone();
            let client = client.clone();

            tokio::spawn(async move {
                refresh_loop(
                    client,
                    refresh_url,
                    subscription_key,
                    api_version,
                    fallback_interval,
                    state,
                )
                .await;
            })
        };

        Ok(Self {
            state,
            _refresh_handle: refresh_handle,
        })
    }

    /// Returns the current `api-token` value.
    ///
    /// This is a cheap read from an `RwLock` that is rarely contended (the
    /// background writer runs once every ~48 minutes).
    pub fn current_api_token(&self) -> String {
        // Use try_read to avoid blocking; fall back to blocking_read which
        // will succeed almost immediately since the writer is very brief.
        match self.state.try_read() {
            Ok(guard) => guard.api_token.clone(),
            Err(_) => {
                // Writer is active (extremely rare, ~ms); block briefly.
                futures::executor::block_on(async { self.state.read().await.api_token.clone() })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn build_refresh_url(base_url: &str, refresh_path: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let path = refresh_path.trim_start_matches('/');
    format!("{base}/{path}")
}

fn read_env_var(name: &str) -> anyhow::Result<String> {
    std::env::var(name)
        .map(|v| v.trim().to_string())
        .map_err(|_| anyhow::anyhow!("Required environment variable {name} is not set"))
        .and_then(|v| {
            if v.is_empty() {
                Err(anyhow::anyhow!(
                    "Required environment variable {name} is empty"
                ))
            } else {
                Ok(v)
            }
        })
}

/// Perform a single token refresh call.
async fn refresh_once(
    client: &Client,
    refresh_url: &str,
    subscription_key: &str,
    api_version: &str,
    current_api_token: &str,
    current_refresh_token: &str,
) -> anyhow::Result<OAuthTokenState> {
    let resp = client
        .post(refresh_url)
        .header("subscription-key", subscription_key)
        .header("api-version", api_version)
        .header("api-token", current_api_token)
        .header("refresh-token", current_refresh_token)
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(
            "Token refresh failed with HTTP {status}: {body}"
        ));
    }

    let refresh_resp: OAuthRefreshResponse = resp.json().await?;

    if !refresh_resp.has_refreshed {
        warn!("Token refresh endpoint returned hasRefreshed=false");
    }

    Ok(OAuthTokenState {
        api_token: refresh_resp.api_token,
        refresh_token: refresh_resp.refresh_token,
        expires_in: refresh_resp.expires_in,
    })
}

/// Compute how long to sleep before the next refresh.
fn next_refresh_delay(expires_in: u64, fallback_interval: u64) -> Duration {
    let effective_ttl = if expires_in > 0 {
        expires_in
    } else {
        fallback_interval
    };
    let delay_secs = (effective_ttl as f64 * REFRESH_BUFFER_FRACTION) as u64;
    Duration::from_secs(delay_secs.max(MIN_RETRY_DELAY_SECS))
}

/// Background loop that refreshes the token before it expires.
async fn refresh_loop(
    client: Client,
    refresh_url: String,
    subscription_key: String,
    api_version: String,
    fallback_interval: u64,
    state: Arc<RwLock<OAuthTokenState>>,
) {
    loop {
        let delay = {
            let guard = state.read().await;
            next_refresh_delay(guard.expires_in, fallback_interval)
        };

        info!("OAuth: next token refresh in {} seconds", delay.as_secs());
        tokio::time::sleep(delay).await;

        // Read current tokens for the refresh request.
        let (current_api_token, current_refresh_token) = {
            let guard = state.read().await;
            (guard.api_token.clone(), guard.refresh_token.clone())
        };

        match refresh_once(
            &client,
            &refresh_url,
            &subscription_key,
            &api_version,
            &current_api_token,
            &current_refresh_token,
        )
        .await
        {
            Ok(new_state) => {
                info!("OAuth: token refreshed successfully");
                let mut guard = state.write().await;
                guard.api_token = new_state.api_token;
                guard.refresh_token = new_state.refresh_token;
                guard.expires_in = new_state.expires_in;
            }
            Err(e) => {
                error!("OAuth: token refresh failed: {e}");
                // Sleep a short interval before retrying.
                tokio::time::sleep(Duration::from_secs(MIN_RETRY_DELAY_SECS)).await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_refresh_url() {
        assert_eq!(
            build_refresh_url("https://proxy.example.com/v1", "tokens/refresh"),
            "https://proxy.example.com/v1/tokens/refresh"
        );
        assert_eq!(
            build_refresh_url("https://proxy.example.com/v1/", "/tokens/refresh"),
            "https://proxy.example.com/v1/tokens/refresh"
        );
    }

    #[test]
    fn test_next_refresh_delay_uses_expires_in() {
        // 3600s * 0.80 = 2880s
        let delay = next_refresh_delay(3600, 1500);
        assert_eq!(delay, Duration::from_secs(2880));
    }

    #[test]
    fn test_next_refresh_delay_falls_back_to_config() {
        // expires_in=0 -> use fallback 1500 * 0.80 = 1200s
        let delay = next_refresh_delay(0, 1500);
        assert_eq!(delay, Duration::from_secs(1200));
    }

    #[test]
    fn test_next_refresh_delay_enforces_minimum() {
        // Very short TTL: 10s * 0.80 = 8s -> clamped to 30s minimum
        let delay = next_refresh_delay(10, 1500);
        assert_eq!(delay, Duration::from_secs(MIN_RETRY_DELAY_SECS));
    }

    #[test]
    fn test_oauth_config_deserializes_from_toml() {
        let toml_str = r#"
refresh_path = "tokens/refresh"
refresh_interval_secs = 1500
api_version = "3.0"
subscription_key_env = "SUBSCRIPTION_KEY"
initial_refresh_token_env = "INITIAL_REFRESH_TOKEN"
initial_api_token_env = "INITIAL_API_TOKEN"
        "#;
        let config: OAuthConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.refresh_path, "tokens/refresh");
        assert_eq!(config.refresh_interval_secs, Some(1500));
        assert_eq!(config.api_version, "3.0");
        assert_eq!(config.subscription_key_env, "SUBSCRIPTION_KEY");
        assert_eq!(config.initial_refresh_token_env, "INITIAL_REFRESH_TOKEN");
        assert_eq!(
            config.initial_api_token_env,
            Some("INITIAL_API_TOKEN".to_string())
        );
    }

    #[test]
    fn test_oauth_config_optional_fields() {
        let toml_str = r#"
refresh_path = "tokens/refresh"
api_version = "3.0"
subscription_key_env = "SUB_KEY"
initial_refresh_token_env = "REFRESH_TOK"
        "#;
        let config: OAuthConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.refresh_interval_secs, None);
        assert_eq!(config.initial_api_token_env, None);
    }

    #[test]
    fn test_refresh_response_deserializes() {
        let json = r#"{
            "apiToken": "tok-abc123",
            "refreshToken": "ref-xyz789",
            "expiresIn": 3600,
            "hasRefreshed": true
        }"#;
        let resp: OAuthRefreshResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.api_token, "tok-abc123");
        assert_eq!(resp.refresh_token, "ref-xyz789");
        assert_eq!(resp.expires_in, 3600);
        assert!(resp.has_refreshed);
    }
}
