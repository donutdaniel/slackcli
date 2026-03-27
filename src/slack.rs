use anyhow::{Context, Result};
use reqwest::{Method, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tracing::debug;

use crate::config::{
    ConfigStore, RuntimeSession, allow_local_api_base_url_override, is_local_host, is_slack_host,
};

#[derive(Debug, Clone)]
pub struct SlackClient {
    http: reqwest::Client,
    base_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackAuthTest {
    pub ok: bool,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub team: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub team_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub bot_id: Option<String>,
    #[serde(default)]
    pub enterprise_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalUploadTicket {
    pub ok: bool,
    pub upload_url: String,
    pub file_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppsConnectionOpen {
    pub ok: bool,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct SlackApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for SlackApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for SlackApiError {}

impl SlackClient {
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(format!("slackcli/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .context("failed to create HTTP client")?;

        Ok(Self {
            http,
            base_url: normalize_base_url(&base_url.into()),
        })
    }

    pub fn from_config(store: &ConfigStore) -> Result<Self> {
        Self::new(store.api_base_url()?)
    }

    pub async fn auth_test(
        &self,
        session: &RuntimeSession,
    ) -> Result<SlackAuthTest, SlackApiError> {
        let value = self
            .request_json(
                session.secret.access_token(),
                Method::POST,
                "auth.test",
                None,
                None,
            )
            .await?;
        serde_json::from_value(value).map_err(|error| SlackApiError {
            status: 500,
            code: "decode_error".into(),
            message: format!("failed to decode Slack auth response: {error}"),
        })
    }

    pub async fn auth_test_for_token(&self, token: &str) -> Result<SlackAuthTest, SlackApiError> {
        let value = self
            .request_json(token, Method::POST, "auth.test", None, None)
            .await?;
        serde_json::from_value(value).map_err(|error| SlackApiError {
            status: 500,
            code: "decode_error".into(),
            message: format!("failed to decode Slack auth response: {error}"),
        })
    }

    pub async fn apps_connections_open_for_token(
        &self,
        token: &str,
    ) -> Result<AppsConnectionOpen, SlackApiError> {
        let value = self
            .request_json(token, Method::POST, "apps.connections.open", None, None)
            .await?;
        serde_json::from_value(value).map_err(|error| SlackApiError {
            status: 500,
            code: "decode_error".into(),
            message: format!("failed to decode Slack Socket Mode open response: {error}"),
        })
    }

    pub async fn users_info(
        &self,
        session: &RuntimeSession,
        user_id: &str,
    ) -> Result<Value, SlackApiError> {
        self.request_json(
            session.secret.access_token(),
            Method::GET,
            "users.info",
            Some(vec![("user".into(), user_id.to_string())]),
            None,
        )
        .await
    }

    pub async fn users_list(
        &self,
        session: &RuntimeSession,
        cursor: Option<String>,
        limit: Option<usize>,
    ) -> Result<Value, SlackApiError> {
        let mut query = Vec::new();
        if let Some(cursor) = cursor {
            query.push(("cursor".into(), cursor));
        }
        if let Some(limit) = limit {
            query.push(("limit".into(), limit.to_string()));
        }

        self.request_json(
            session.secret.access_token(),
            Method::GET,
            "users.list",
            (!query.is_empty()).then_some(query),
            None,
        )
        .await
    }

    pub async fn users_conversations(
        &self,
        session: &RuntimeSession,
        query: Vec<(String, String)>,
    ) -> Result<Value, SlackApiError> {
        self.request_json(
            session.secret.access_token(),
            Method::GET,
            "users.conversations",
            Some(query),
            None,
        )
        .await
    }

    pub async fn conversations_info(
        &self,
        session: &RuntimeSession,
        channel: &str,
        include_num_members: bool,
    ) -> Result<Value, SlackApiError> {
        let mut query = vec![("channel".into(), channel.to_string())];
        if include_num_members {
            query.push(("include_num_members".into(), "true".into()));
        }
        self.request_json(
            session.secret.access_token(),
            Method::GET,
            "conversations.info",
            Some(query),
            None,
        )
        .await
    }

    pub async fn conversations_history(
        &self,
        session: &RuntimeSession,
        query: Vec<(String, String)>,
    ) -> Result<Value, SlackApiError> {
        self.request_json(
            session.secret.access_token(),
            Method::GET,
            "conversations.history",
            Some(query),
            None,
        )
        .await
    }

    pub async fn conversations_replies(
        &self,
        session: &RuntimeSession,
        query: Vec<(String, String)>,
    ) -> Result<Value, SlackApiError> {
        self.request_json(
            session.secret.access_token(),
            Method::GET,
            "conversations.replies",
            Some(query),
            None,
        )
        .await
    }

    pub async fn conversations_members(
        &self,
        session: &RuntimeSession,
        channel: &str,
        cursor: Option<String>,
        limit: Option<usize>,
    ) -> Result<Value, SlackApiError> {
        let mut query = vec![("channel".into(), channel.to_string())];
        if let Some(cursor) = cursor {
            query.push(("cursor".into(), cursor));
        }
        if let Some(limit) = limit {
            query.push(("limit".into(), limit.to_string()));
        }

        self.request_json(
            session.secret.access_token(),
            Method::GET,
            "conversations.members",
            Some(query),
            None,
        )
        .await
    }

    pub async fn conversations_open(
        &self,
        session: &RuntimeSession,
        users: &[String],
        return_im: bool,
        prevent_creation: bool,
    ) -> Result<Value, SlackApiError> {
        let mut body = serde_json::json!({
            "users": users.join(","),
        });

        if return_im && let Some(object) = body.as_object_mut() {
            object.insert("return_im".into(), Value::Bool(true));
        }
        if prevent_creation && let Some(object) = body.as_object_mut() {
            object.insert("prevent_creation".into(), Value::Bool(true));
        }

        self.request_json(
            session.secret.access_token(),
            Method::POST,
            "conversations.open",
            None,
            Some(body),
        )
        .await
    }

    pub async fn conversations_create(
        &self,
        session: &RuntimeSession,
        name: &str,
        is_private: bool,
    ) -> Result<Value, SlackApiError> {
        let mut body = serde_json::json!({
            "name": name,
        });
        if is_private && let Some(object) = body.as_object_mut() {
            object.insert("is_private".into(), Value::Bool(true));
        }

        self.request_json(
            session.secret.access_token(),
            Method::POST,
            "conversations.create",
            None,
            Some(body),
        )
        .await
    }

    pub async fn conversations_archive(
        &self,
        session: &RuntimeSession,
        channel: &str,
    ) -> Result<Value, SlackApiError> {
        self.request_json(
            session.secret.access_token(),
            Method::POST,
            "conversations.archive",
            None,
            Some(serde_json::json!({
                "channel": channel,
            })),
        )
        .await
    }

    pub async fn team_info(&self, session: &RuntimeSession) -> Result<Value, SlackApiError> {
        self.request_json(
            session.secret.access_token(),
            Method::GET,
            "team.info",
            None,
            None,
        )
        .await
    }

    pub async fn search_messages(
        &self,
        session: &RuntimeSession,
        query: Vec<(String, String)>,
    ) -> Result<Value, SlackApiError> {
        self.request_json(
            session.secret.access_token(),
            Method::GET,
            "search.messages",
            Some(query),
            None,
        )
        .await
    }

    pub async fn reactions_add(
        &self,
        session: &RuntimeSession,
        channel: &str,
        ts: &str,
        name: &str,
    ) -> Result<Value, SlackApiError> {
        self.request_json(
            session.secret.access_token(),
            Method::POST,
            "reactions.add",
            None,
            Some(serde_json::json!({
                "channel": channel,
                "timestamp": ts,
                "name": name,
            })),
        )
        .await
    }

    pub async fn reactions_remove(
        &self,
        session: &RuntimeSession,
        channel: &str,
        ts: &str,
        name: &str,
    ) -> Result<Value, SlackApiError> {
        self.request_json(
            session.secret.access_token(),
            Method::POST,
            "reactions.remove",
            None,
            Some(serde_json::json!({
                "channel": channel,
                "timestamp": ts,
                "name": name,
            })),
        )
        .await
    }

    pub async fn reactions_list(
        &self,
        session: &RuntimeSession,
        query: Vec<(String, String)>,
    ) -> Result<Value, SlackApiError> {
        self.request_json(
            session.secret.access_token(),
            Method::GET,
            "reactions.list",
            Some(query),
            None,
        )
        .await
    }

    pub async fn chat_post_message(
        &self,
        session: &RuntimeSession,
        body: Value,
    ) -> Result<Value, SlackApiError> {
        self.request_json(
            session.secret.access_token(),
            Method::POST,
            "chat.postMessage",
            None,
            Some(body),
        )
        .await
    }

    pub async fn chat_update(
        &self,
        session: &RuntimeSession,
        body: Value,
    ) -> Result<Value, SlackApiError> {
        self.request_json(
            session.secret.access_token(),
            Method::POST,
            "chat.update",
            None,
            Some(body),
        )
        .await
    }

    pub async fn chat_delete(
        &self,
        session: &RuntimeSession,
        channel: &str,
        ts: &str,
    ) -> Result<Value, SlackApiError> {
        self.request_json(
            session.secret.access_token(),
            Method::POST,
            "chat.delete",
            None,
            Some(serde_json::json!({
                "channel": channel,
                "ts": ts,
            })),
        )
        .await
    }

    pub async fn chat_get_permalink(
        &self,
        session: &RuntimeSession,
        channel: &str,
        ts: &str,
    ) -> Result<Value, SlackApiError> {
        self.request_json(
            session.secret.access_token(),
            Method::GET,
            "chat.getPermalink",
            Some(vec![
                ("channel".into(), channel.to_string()),
                ("message_ts".into(), ts.to_string()),
            ]),
            None,
        )
        .await
    }

    pub async fn files_list(
        &self,
        session: &RuntimeSession,
        query: Vec<(String, String)>,
    ) -> Result<Value, SlackApiError> {
        self.request_json(
            session.secret.access_token(),
            Method::GET,
            "files.list",
            Some(query),
            None,
        )
        .await
    }

    pub async fn files_info(
        &self,
        session: &RuntimeSession,
        file_id: &str,
    ) -> Result<Value, SlackApiError> {
        self.request_json(
            session.secret.access_token(),
            Method::GET,
            "files.info",
            Some(vec![("file".into(), file_id.to_string())]),
            None,
        )
        .await
    }

    pub async fn files_delete(
        &self,
        session: &RuntimeSession,
        file_id: &str,
    ) -> Result<Value, SlackApiError> {
        self.request_json(
            session.secret.access_token(),
            Method::POST,
            "files.delete",
            None,
            Some(serde_json::json!({
                "file": file_id,
            })),
        )
        .await
    }

    pub async fn files_get_upload_url_external(
        &self,
        session: &RuntimeSession,
        filename: &str,
        length: u64,
    ) -> Result<ExternalUploadTicket, SlackApiError> {
        let value = self
            .request_form(
                session.secret.access_token(),
                Method::POST,
                "files.getUploadURLExternal",
                vec![
                    ("filename".into(), filename.to_string()),
                    ("length".into(), length.to_string()),
                ],
            )
            .await?;
        serde_json::from_value(value).map_err(|error| SlackApiError {
            status: 500,
            code: "decode_error".into(),
            message: format!("failed to decode Slack file upload URL response: {error}"),
        })
    }

    pub async fn upload_external_file(
        &self,
        upload_url: &str,
        bytes: Vec<u8>,
    ) -> Result<(), SlackApiError> {
        let upload_url = validate_external_upload_url(upload_url)?;
        let response = self
            .http
            .request(Method::POST, upload_url)
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(bytes)
            .send()
            .await
            .map_err(|error| SlackApiError {
                status: 502,
                code: "network_error".into(),
                message: format!("failed to upload bytes to Slack's external upload URL: {error}"),
            })?;

        let status = response.status();
        if status.is_success() {
            return Ok(());
        }

        let body = response.text().await.unwrap_or_default();
        Err(SlackApiError {
            status: status.as_u16(),
            code: "file_upload_failed".into(),
            message: format!(
                "Slack external upload failed with HTTP status {status}: {}",
                body.trim()
            ),
        })
    }

    pub async fn files_complete_upload_external(
        &self,
        session: &RuntimeSession,
        file_id: &str,
        title: Option<&str>,
        channel_id: Option<&str>,
        initial_comment: Option<&str>,
        thread_ts: Option<&str>,
    ) -> Result<Value, SlackApiError> {
        let mut files_payload = vec![serde_json::json!({
            "id": file_id,
        })];
        if let Some(title) = title
            && let Some(object) = files_payload[0].as_object_mut()
        {
            object.insert("title".into(), Value::String(title.to_string()));
        }

        let mut form = vec![(
            "files".to_string(),
            serde_json::to_string(&files_payload).map_err(|error| SlackApiError {
                status: 500,
                code: "encode_error".into(),
                message: format!("failed to encode Slack file completion payload: {error}"),
            })?,
        )];

        if let Some(channel_id) = channel_id {
            form.push(("channel_id".into(), channel_id.to_string()));
        }
        if let Some(initial_comment) = initial_comment {
            form.push(("initial_comment".into(), initial_comment.to_string()));
        }
        if let Some(thread_ts) = thread_ts {
            form.push(("thread_ts".into(), thread_ts.to_string()));
        }

        self.request_form(
            session.secret.access_token(),
            Method::POST,
            "files.completeUploadExternal",
            form,
        )
        .await
    }

    pub async fn api_call(
        &self,
        session: &RuntimeSession,
        http_method: Method,
        api_method: &str,
        query: Option<Vec<(String, String)>>,
        body: Option<Value>,
    ) -> Result<Value, SlackApiError> {
        self.request_json(
            session.secret.access_token(),
            http_method,
            api_method,
            query,
            body,
        )
        .await
    }

    async fn request_json(
        &self,
        token: &str,
        http_method: Method,
        api_method: &str,
        query: Option<Vec<(String, String)>>,
        body: Option<Value>,
    ) -> Result<Value, SlackApiError> {
        let url = format!("{}/{}", self.base_url, api_method);
        let mut request = self
            .http
            .request(http_method.clone(), &url)
            .bearer_auth(token)
            .header(reqwest::header::ACCEPT, "application/json");

        if let Some(query) = &query {
            request = request.query(query);
        }

        if let Some(body) = body {
            request = request
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "application/json; charset=utf-8",
                )
                .json(&body);
        }

        debug!(method = %http_method, api_method, url, "sending Slack API request");
        let response = request.send().await.map_err(|error| SlackApiError {
            status: 502,
            code: "network_error".into(),
            message: format!("failed to send request to Slack: {error}"),
        })?;

        decode_response(response).await
    }

    async fn request_form(
        &self,
        token: &str,
        http_method: Method,
        api_method: &str,
        form: Vec<(String, String)>,
    ) -> Result<Value, SlackApiError> {
        let url = format!("{}/{}", self.base_url, api_method);
        let encoded = serde_urlencoded::to_string(&form).map_err(|error| SlackApiError {
            status: 500,
            code: "encode_error".into(),
            message: format!("failed to encode Slack form request: {error}"),
        })?;
        let request = self
            .http
            .request(http_method.clone(), &url)
            .bearer_auth(token)
            .header(reqwest::header::ACCEPT, "application/json")
            // Slack's upload form endpoints emit `superfluous_charset` when charset is present.
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(encoded);

        debug!(method = %http_method, api_method, url, "sending Slack form API request");
        let response = request.send().await.map_err(|error| SlackApiError {
            status: 502,
            code: "network_error".into(),
            message: format!("failed to send form request to Slack: {error}"),
        })?;

        decode_response(response).await
    }
}

pub fn merge_object_body(
    body: Option<Value>,
    params: &[(String, String)],
) -> Result<Option<Value>, SlackApiError> {
    if params.is_empty() {
        return Ok(body);
    }

    let mut map = match body {
        None => Map::new(),
        Some(Value::Object(map)) => map,
        Some(_) => {
            return Err(SlackApiError {
                status: 400,
                code: "validation_error".into(),
                message: "request body must be a JSON object when used with --param".into(),
            });
        }
    };

    for (key, value) in params {
        map.insert(key.clone(), Value::String(value.clone()));
    }

    Ok(Some(Value::Object(map)))
}

async fn decode_response(response: Response) -> Result<Value, SlackApiError> {
    let status = response.status();
    let retry_after = retry_after_seconds(response.headers());
    let text = response.text().await.map_err(|error| SlackApiError {
        status: 502,
        code: "network_error".into(),
        message: format!("failed to read Slack response body: {error}"),
    })?;

    if status == StatusCode::TOO_MANY_REQUESTS {
        let message = if let Some(retry_after) = retry_after {
            format!("Slack API rate limited the request; retry after {retry_after} seconds")
        } else {
            "Slack API rate limited the request".to_string()
        };

        return Err(SlackApiError {
            status: 429,
            code: "rate_limited".into(),
            message,
        });
    }

    let parsed: Value = serde_json::from_str(&text).map_err(|error| SlackApiError {
        status: status.as_u16().max(500),
        code: "decode_error".into(),
        message: format!("Slack returned a non-JSON response: {error}"),
    })?;

    if !status.is_success() {
        let code = parsed
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("http_error")
            .to_string();
        let message = parsed
            .get("error")
            .and_then(Value::as_str)
            .map(|error| format!("Slack rejected the request with {status}: {error}"))
            .unwrap_or_else(|| format!("Slack rejected the request with HTTP status {status}"));

        return Err(SlackApiError {
            status: status.as_u16(),
            code,
            message,
        });
    }

    match parsed.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(parsed),
        Some(false) => {
            let code = parsed
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("slack_api_error")
                .to_string();
            let needed = parsed.get("needed").and_then(Value::as_str);
            let provided = parsed.get("provided").and_then(Value::as_str);

            let mut message = format!("Slack API returned `{code}`");
            if let Some(needed) = needed {
                message.push_str(&format!("; needed scopes: {needed}"));
            }
            if let Some(provided) = provided {
                message.push_str(&format!("; provided scopes: {provided}"));
            }

            let status = match code.as_str() {
                "invalid_auth" | "not_authed" | "account_inactive" | "token_revoked"
                | "token_expired" => 401,
                "missing_scope" | "no_permission" | "not_allowed_token_type" => 403,
                "channel_not_found" | "user_not_found" | "message_not_found"
                | "thread_not_found" | "file_not_found" | "no_reaction" => 404,
                "already_reacted" => 409,
                "ratelimited" => 429,
                _ => 400,
            };

            Err(SlackApiError {
                status,
                code,
                message,
            })
        }
        None => Ok(parsed),
    }
}

fn normalize_base_url(raw: &str) -> String {
    raw.trim_end_matches('/').to_string()
}

fn validate_external_upload_url(upload_url: &str) -> Result<Url, SlackApiError> {
    let parsed = Url::parse(upload_url).map_err(|error| SlackApiError {
        status: 400,
        code: "unsafe_url".into(),
        message: format!("Slack returned an invalid external upload URL: {error}"),
    })?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(SlackApiError {
            status: 400,
            code: "unsafe_url".into(),
            message: "Slack returned an unsafe external upload URL with embedded credentials"
                .into(),
        });
    }

    let host = parsed.host_str().ok_or_else(|| SlackApiError {
        status: 400,
        code: "unsafe_url".into(),
        message: "Slack returned an external upload URL without a host".into(),
    })?;

    if is_local_host(host) {
        if !allow_local_api_base_url_override() {
            return Err(SlackApiError {
                status: 400,
                code: "unsafe_url".into(),
                message: "refusing to upload bytes to a localhost URL unless SLACKCLI_UNSAFE_ALLOW_LOCAL_API_BASE_URL=1".into(),
            });
        }
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(SlackApiError {
                status: 400,
                code: "unsafe_url".into(),
                message: "localhost upload URLs must use http or https".into(),
            });
        }
        return Ok(parsed);
    }

    if parsed.scheme() != "https" {
        return Err(SlackApiError {
            status: 400,
            code: "unsafe_url".into(),
            message: "Slack external upload URLs must use https".into(),
        });
    }
    if !is_slack_host(host) {
        return Err(SlackApiError {
            status: 400,
            code: "unsafe_url".into(),
            message: format!("refusing to upload bytes to non-Slack host `{host}`"),
        });
    }
    if let Some(port) = parsed.port()
        && port != 443
    {
        return Err(SlackApiError {
            status: 400,
            code: "unsafe_url".into(),
            message: "Slack external upload URLs must use port 443".into(),
        });
    }

    Ok(parsed)
}

fn retry_after_seconds(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{RuntimeSession, SessionSource, StoredSecret};
    use httpmock::prelude::*;

    #[test]
    fn merges_params_into_body() -> Result<()> {
        let body = Some(serde_json::json!({"channel":"C123"}));
        let merged = merge_object_body(body, &[("text".into(), "hello".into())])?;
        assert_eq!(
            merged,
            Some(serde_json::json!({"channel":"C123","text":"hello"}))
        );
        Ok(())
    }

    #[test]
    fn rejects_non_object_body_for_merge() {
        let error =
            merge_object_body(Some(Value::Bool(true)), &[("x".into(), "y".into())]).unwrap_err();
        assert_eq!(error.status, 400);
    }

    fn test_session() -> RuntimeSession {
        RuntimeSession {
            profile_name: Some("test".into()),
            secret: StoredSecret::Token {
                token: "xoxp-test-token".into(),
            },
            source: SessionSource::PersistedProfile,
        }
    }

    #[tokio::test]
    async fn apps_connections_open_uses_bearer_auth() -> Result<()> {
        let server = MockServer::start_async().await;
        let method_mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/apps.connections.open")
                    .header("authorization", "Bearer xapp-test-token");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(serde_json::json!({
                        "ok": true,
                        "url": "wss://wss.slack.com/link/?ticket=abc"
                    }));
            })
            .await;

        let client = SlackClient::new(server.base_url())?;
        let response = client
            .apps_connections_open_for_token("xapp-test-token")
            .await?;

        assert_eq!(response.url, "wss://wss.slack.com/link/?ticket=abc");
        method_mock.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn api_call_sends_auth_and_query() -> Result<()> {
        let server = MockServer::start_async().await;
        let method_mock = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/auth.test")
                    .header("authorization", "Bearer xoxp-test-token")
                    .query_param("limit", "5");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(serde_json::json!({"ok": true, "echo": "works"}));
            })
            .await;

        let client = SlackClient::new(server.base_url())?;
        let value = client
            .api_call(
                &test_session(),
                Method::GET,
                "auth.test",
                Some(vec![("limit".into(), "5".into())]),
                None,
            )
            .await?;

        assert_eq!(value.get("echo").and_then(Value::as_str), Some("works"));
        method_mock.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn api_call_surfaces_missing_scope_details() -> Result<()> {
        let server = MockServer::start_async().await;
        let method_mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/chat.postMessage")
                    .header("content-type", "application/json; charset=utf-8");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(serde_json::json!({
                        "ok": false,
                        "error": "missing_scope",
                        "needed": "chat:write",
                        "provided": "channels:read"
                    }));
            })
            .await;

        let client = SlackClient::new(server.base_url())?;
        let error = client
            .api_call(
                &test_session(),
                Method::POST,
                "chat.postMessage",
                None,
                Some(serde_json::json!({"channel":"C123","text":"hello"})),
            )
            .await
            .unwrap_err();

        assert_eq!(error.status, 403);
        assert_eq!(error.code, "missing_scope");
        assert!(error.message.contains("needed scopes: chat:write"));
        assert!(error.message.contains("provided scopes: channels:read"));
        method_mock.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn api_call_translates_rate_limits() -> Result<()> {
        let server = MockServer::start_async().await;
        let method_mock = server
            .mock_async(|when, then| {
                when.method(GET).path("/conversations.history");
                then.status(429)
                    .header("retry-after", "17")
                    .body("too many requests");
            })
            .await;

        let client = SlackClient::new(server.base_url())?;
        let error = client
            .api_call(
                &test_session(),
                Method::GET,
                "conversations.history",
                Some(vec![("channel".into(), "C123".into())]),
                None,
            )
            .await
            .unwrap_err();

        assert_eq!(error.status, 429);
        assert_eq!(error.code, "rate_limited");
        assert!(error.message.contains("retry after 17 seconds"));
        method_mock.assert_async().await;
        Ok(())
    }

    #[tokio::test]
    async fn files_complete_upload_external_encodes_form_fields() -> Result<()> {
        let server = MockServer::start_async().await;
        let method_mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path("/files.completeUploadExternal")
                    .header("authorization", "Bearer xoxp-test-token")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body_contains("channel_id=C123")
                    .body_contains("initial_comment=hello")
                    .body_contains("thread_ts=1710000000.000100")
                    .body_contains("title");
                then.status(200)
                    .header("content-type", "application/json")
                    .json_body(serde_json::json!({"ok": true}));
            })
            .await;

        let client = SlackClient::new(server.base_url())?;
        let value = client
            .files_complete_upload_external(
                &test_session(),
                "F123",
                Some("report"),
                Some("C123"),
                Some("hello"),
                Some("1710000000.000100"),
            )
            .await?;

        assert_eq!(value.get("ok").and_then(Value::as_bool), Some(true));
        method_mock.assert_async().await;
        Ok(())
    }

    #[test]
    fn rejects_non_slack_external_upload_urls() {
        let error = validate_external_upload_url("https://example.com/upload").unwrap_err();
        assert_eq!(error.code, "unsafe_url");
        assert!(error.message.contains("non-Slack host"));
    }

    #[test]
    fn accepts_slack_external_upload_urls() -> Result<()> {
        let parsed = validate_external_upload_url("https://files.slack.com/upload/v1/test")?;
        assert_eq!(parsed.host_str(), Some("files.slack.com"));
        Ok(())
    }
}
