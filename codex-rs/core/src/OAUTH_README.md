# OAuth Token Refresh for Medtronic Proxy

This document describes how to configure Codex CLI to work with the Medtronic
company proxy, which secures its OpenAI-compatible Responses API behind an OAuth
token that expires periodically.

## Overview

The Medtronic proxy requires three custom headers on every API request:

| Header             | Type    | Description                                      |
|--------------------|---------|--------------------------------------------------|
| `subscription-key` | Static  | Your Medtronic API subscription key               |
| `api-version`      | Static  | API version (e.g. `3.0`)                          |
| `api-token`        | Dynamic | OAuth token that expires (e.g. every 60 minutes)  |

To refresh the `api-token`, the proxy exposes a refresh endpoint that accepts
a `POST` request with **no body** and four headers:

| Header             | Value                                        |
|--------------------|----------------------------------------------|
| `subscription-key` | Your static subscription key                  |
| `api-version`      | Same api-version as above                     |
| `api-token`        | The current (possibly expired) api-token      |
| `refresh-token`    | The current refresh token                     |

The refresh endpoint returns:

```json
{
    "apiToken": "<new-api-token>",
    "refreshToken": "<new-refresh-token>",
    "expiresIn": 3600,
    "hasRefreshed": true
}
```

## How It Works

Codex now has a built-in `OAuthTokenManager` (in `codex-rs/core/src/oauth.rs`)
that:

1. **On startup**: reads your initial credentials from environment variables and
   calls the refresh endpoint to obtain the first valid `api-token`.
2. **Background refresh**: spawns a background task that proactively refreshes
   the token at 80% of the `expiresIn` TTL (e.g. at 48 minutes for a 60-minute
   token), well before it expires.
3. **Per-turn injection**: on every Codex turn (each time you send a prompt),
   the fresh `api-token` is injected as a header into the outgoing request
   alongside the static `subscription-key` and `api-version` headers.

No separate proxy process is needed -- Codex talks directly to the Medtronic
proxy.

## Setup

### 1. Set Environment Variables

```bash
export SUBSCRIPTION_KEY="your-medtronic-subscription-key"
export INITIAL_REFRESH_TOKEN="your-initial-refresh-token"
export INITIAL_API_TOKEN="your-initial-api-token"
```

- `SUBSCRIPTION_KEY`: Your Medtronic API subscription key (static, does not
  change).
- `INITIAL_REFRESH_TOKEN`: A valid refresh token to bootstrap the first token
  exchange. After the first refresh, the manager uses the new refresh token
  returned by the server.
- `INITIAL_API_TOKEN`: A valid (or recently expired) api-token for the first
  refresh call. After that, the manager uses the refreshed token.

### 2. Configure `~/.codex/config.toml`

```toml
model = "your-model-name"
model_provider = "medtronic"

[model_providers.medtronic]
name = "Medtronic Proxy"
base_url = "https://your-medtronic-proxy-url.com"
wire_api = "responses"
requires_openai_auth = false

# Static headers sent on every Responses API request
http_headers = { "subscription-key" = "your-sub-key-value", "api-version" = "3.0" }

# OAuth token refresh configuration
[model_providers.medtronic.oauth]
refresh_path = "tokens/refresh"
api_version = "3.0"
subscription_key_env = "SUBSCRIPTION_KEY"
initial_refresh_token_env = "INITIAL_REFRESH_TOKEN"
initial_api_token_env = "INITIAL_API_TOKEN"

# Optional: override the fallback refresh interval (seconds).
# Only used if the server doesn't return a usable expiresIn value.
# Default is 1500 (25 minutes).
# refresh_interval_secs = 1500
```

### 3. Run Codex

```bash
codex
```

Codex will:
1. Read your environment variables.
2. Call the refresh endpoint to get a valid `api-token`.
3. Start the background refresh loop.
4. Route all Responses API requests to the Medtronic proxy with the correct
   headers.

## Troubleshooting

### "Required environment variable ... is not set"

Make sure `SUBSCRIPTION_KEY`, `INITIAL_REFRESH_TOKEN`, and `INITIAL_API_TOKEN`
are exported in your shell before running Codex.

### "Token refresh failed with HTTP 401"

Your `INITIAL_REFRESH_TOKEN` or `INITIAL_API_TOKEN` may have expired. Obtain
fresh values from your Medtronic portal and re-export them.

### "Failed to initialize OAuth token manager"

Check that:
- The `base_url` in your config points to the correct Medtronic proxy URL.
- The `refresh_path` is correct (e.g. `tokens/refresh`).
- Network connectivity to the proxy is available.

## Files Changed

| File | Description |
|------|-------------|
| `core/src/oauth.rs` | New: `OAuthConfig`, `OAuthTokenManager`, background refresh loop |
| `core/src/lib.rs` | Register `oauth` module |
| `core/src/model_provider_info.rs` | Add `oauth: Option<OAuthConfig>` field to `ModelProviderInfo` |
| `core/src/client.rs` | Lazy-init `OAuthTokenManager`, inject `api-token` header per-turn |
| `core/config.schema.json` | Regenerated to include `OAuthConfig` schema |
