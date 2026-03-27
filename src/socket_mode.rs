use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use reqwest::Url;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::time::{Duration, sleep};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::config::{allow_local_api_base_url_override, is_local_host, is_slack_host};
use crate::output::OutputFormat;
use crate::slack::SlackClient;

type SocketStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone, Copy)]
pub struct ListenOptions {
    pub output: OutputFormat,
    pub debug_reconnects: bool,
    pub reconnect_delay: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionAction {
    Continue,
    Reconnect,
    Shutdown,
}

pub async fn listen(client: &SlackClient, app_token: &str, options: ListenOptions) -> Result<()> {
    loop {
        let response = client.apps_connections_open_for_token(app_token).await?;
        let socket_url = prepare_socket_url(&response.url, options.debug_reconnects)?;

        let (websocket, _) = connect_async(socket_url.as_str())
            .await
            .context("failed to connect to Slack Socket Mode WebSocket")?;
        match run_connection(websocket, options.output).await? {
            ConnectionAction::Continue => {}
            ConnectionAction::Reconnect => sleep(options.reconnect_delay).await,
            ConnectionAction::Shutdown => return Ok(()),
        }
    }
}

async fn run_connection(
    mut websocket: SocketStream,
    output: OutputFormat,
) -> Result<ConnectionAction> {
    loop {
        let next = tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                let _ = websocket.close(None).await;
                return Ok(ConnectionAction::Shutdown);
            }
            next = websocket.next() => next,
        };

        let Some(message) = next else {
            return Ok(ConnectionAction::Reconnect);
        };

        match message.context("Slack Socket Mode connection failed")? {
            Message::Text(text) => {
                match handle_text_message(&mut websocket, text.as_ref(), output).await? {
                    ConnectionAction::Continue => {}
                    action => return Ok(action),
                }
            }
            Message::Binary(bytes) => {
                let text = std::str::from_utf8(&bytes)
                    .context("received a non-UTF-8 Socket Mode frame from Slack")?;
                match handle_text_message(&mut websocket, text, output).await? {
                    ConnectionAction::Continue => {}
                    action => return Ok(action),
                }
            }
            Message::Ping(payload) => {
                websocket
                    .send(Message::Pong(payload))
                    .await
                    .context("failed to respond to Slack Socket Mode ping")?;
            }
            Message::Pong(_) => {}
            Message::Close(frame) => {
                if let Some(frame) = frame {
                    output.print_success(&json!({
                        "type": "websocket_close",
                        "code": u16::from(frame.code),
                        "reason": frame.reason.to_string(),
                    }))?;
                }
                return Ok(ConnectionAction::Reconnect);
            }
            _ => {}
        }
    }
}

async fn handle_text_message(
    websocket: &mut SocketStream,
    text: &str,
    output: OutputFormat,
) -> Result<ConnectionAction> {
    let value: Value =
        serde_json::from_str(text).context("failed to parse Slack Socket Mode frame as JSON")?;

    if let Some(envelope_id) = value.get("envelope_id").and_then(Value::as_str) {
        send_ack(websocket, envelope_id).await?;
    }

    output.print_success(&value)?;

    if value.get("type").and_then(Value::as_str) != Some("disconnect") {
        return Ok(ConnectionAction::Continue);
    }

    match value.get("reason").and_then(Value::as_str) {
        Some("warning") => Ok(ConnectionAction::Continue),
        Some("link_disabled") => bail!(
            "Slack Socket Mode was disabled for this app; re-enable Socket Mode and rerun `slackcli listen`"
        ),
        Some(_) | None => Ok(ConnectionAction::Reconnect),
    }
}

async fn send_ack(websocket: &mut SocketStream, envelope_id: &str) -> Result<()> {
    let ack = json!({ "envelope_id": envelope_id });
    let ack = serde_json::to_string(&ack).context("failed to serialize Socket Mode ack")?;
    websocket
        .send(Message::Text(ack.into()))
        .await
        .context("failed to acknowledge Slack Socket Mode envelope")?;
    Ok(())
}

fn prepare_socket_url(raw: &str, debug_reconnects: bool) -> Result<Url> {
    let mut url = Url::parse(raw)
        .with_context(|| format!("Slack returned an invalid Socket Mode WebSocket URL: {raw}"))?;

    if !url.username().is_empty() || url.password().is_some() {
        bail!("Slack returned an unsafe Socket Mode WebSocket URL with embedded credentials");
    }

    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("Slack returned a Socket Mode WebSocket URL without a host"))?;

    if is_local_host(host) {
        if !allow_local_api_base_url_override() {
            bail!(
                "refusing to connect to a localhost Socket Mode URL unless SLACKCLI_UNSAFE_ALLOW_LOCAL_API_BASE_URL=1"
            );
        }
        if !matches!(url.scheme(), "ws" | "wss") {
            bail!("localhost Socket Mode URLs must use ws or wss");
        }
    } else {
        if url.scheme() != "wss" {
            bail!("Slack Socket Mode WebSocket URLs must use wss");
        }
        if !is_slack_host(host) {
            bail!("refusing to connect to non-Slack Socket Mode host `{host}`");
        }
        if let Some(port) = url.port()
            && port != 443
        {
            bail!("Slack Socket Mode WebSocket URLs must use port 443");
        }
    }

    if debug_reconnects {
        url.query_pairs_mut()
            .append_pair("debug_reconnects", "true");
    }

    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_debug_reconnects_query_flag() -> Result<()> {
        let url = prepare_socket_url("wss://wss.slack.com/link/?ticket=abc", true)?;
        assert_eq!(
            url.query_pairs().find(|(key, _)| key == "debug_reconnects"),
            Some(("debug_reconnects".into(), "true".into()))
        );
        Ok(())
    }

    #[test]
    fn rejects_non_slack_host() {
        let error = prepare_socket_url("wss://example.com/link/?ticket=abc", false).unwrap_err();
        assert!(error.to_string().contains("non-Slack Socket Mode host"));
    }

    #[test]
    fn disconnect_warning_keeps_connection_alive() -> Result<()> {
        let value = serde_json::json!({
            "type": "disconnect",
            "reason": "warning",
        });
        assert_eq!(
            handle_disconnect_for_test(&value)?,
            ConnectionAction::Continue
        );
        Ok(())
    }

    #[test]
    fn disconnect_refresh_requests_reconnect() -> Result<()> {
        let value = serde_json::json!({
            "type": "disconnect",
            "reason": "refresh_requested",
        });
        assert_eq!(
            handle_disconnect_for_test(&value)?,
            ConnectionAction::Reconnect
        );
        Ok(())
    }

    fn handle_disconnect_for_test(value: &Value) -> Result<ConnectionAction> {
        if value.get("type").and_then(Value::as_str) != Some("disconnect") {
            return Ok(ConnectionAction::Continue);
        }

        match value.get("reason").and_then(Value::as_str) {
            Some("warning") => Ok(ConnectionAction::Continue),
            Some("link_disabled") => bail!(
                "Slack Socket Mode was disabled for this app; re-enable Socket Mode and rerun `slackcli listen`"
            ),
            Some(_) | None => Ok(ConnectionAction::Reconnect),
        }
    }
}
