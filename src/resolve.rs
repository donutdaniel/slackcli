use anyhow::{Result, anyhow, bail};
use serde_json::Value;

use crate::config::RuntimeSession;
use crate::slack::SlackClient;

#[derive(Default)]
pub struct SlackResolver {
    users: Option<Vec<Value>>,
    conversations: Option<Vec<Value>>,
    auth_user_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DmResolutionMode {
    ExistingOnly,
    AllowOpen,
}

impl SlackResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn auth_user_id(
        &mut self,
        client: &SlackClient,
        session: &RuntimeSession,
    ) -> Result<String> {
        if let Some(user_id) = &self.auth_user_id {
            return Ok(user_id.clone());
        }

        let auth = client.auth_test(session).await?;
        let user_id = auth
            .user_id
            .ok_or_else(|| anyhow!("Slack auth response did not include a user_id"))?;
        self.auth_user_id = Some(user_id.clone());
        Ok(user_id)
    }

    pub async fn resolve_user_id(
        &mut self,
        client: &SlackClient,
        session: &RuntimeSession,
        raw: &str,
    ) -> Result<String> {
        let trimmed = raw.trim();
        if looks_like_user_id(trimmed) {
            return Ok(trimmed.to_string());
        }

        let normalized = normalize_user_reference(trimmed);
        if normalized.is_empty() {
            bail!("could not resolve Slack user `{raw}`")
        }

        if normalized == "me" {
            return self.auth_user_id(client, session).await;
        }

        let users = self.load_users(client, session).await?;
        let mut matches: Vec<(String, String)> = Vec::new();

        for user in users {
            if user_matches(user, &normalized)
                && let Some(id) = user.get("id").and_then(Value::as_str)
            {
                matches.push((id.to_string(), summarize_user(user)));
            }
        }

        matches.sort_by(|left, right| left.0.cmp(&right.0));
        matches.dedup_by(|left, right| left.0 == right.0);

        match matches.as_slice() {
            [(id, _summary)] => Ok(id.clone()),
            [] => bail!("could not resolve Slack user `{raw}`"),
            many => {
                let options = many
                    .iter()
                    .take(5)
                    .map(|(_, summary)| summary.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!("multiple Slack users matched `{raw}`: {options}")
            }
        }
    }

    pub async fn resolve_conversation_id(
        &mut self,
        client: &SlackClient,
        session: &RuntimeSession,
        raw: &str,
    ) -> Result<String> {
        self.resolve_conversation_id_with_mode(client, session, raw, DmResolutionMode::ExistingOnly)
            .await
    }

    pub async fn resolve_conversation_id_for_write(
        &mut self,
        client: &SlackClient,
        session: &RuntimeSession,
        raw: &str,
    ) -> Result<String> {
        self.resolve_conversation_id_with_mode(client, session, raw, DmResolutionMode::AllowOpen)
            .await
    }

    async fn resolve_conversation_id_with_mode(
        &mut self,
        client: &SlackClient,
        session: &RuntimeSession,
        raw: &str,
        dm_resolution: DmResolutionMode,
    ) -> Result<String> {
        let trimmed = raw.trim();
        if looks_like_conversation_id(trimmed) {
            return Ok(trimmed.to_string());
        }

        if looks_like_user_id(trimmed) || trimmed.starts_with('@') {
            let user_id = self.resolve_user_id(client, session, trimmed).await?;
            let current_user_id = self.auth_user_id(client, session).await?;
            if user_id == current_user_id {
                bail!("`@me` is not a valid DM target; provide another user")
            }

            if let Some(channel_id) = self
                .find_existing_im_with_user(client, session, &user_id)
                .await?
            {
                return Ok(channel_id);
            }

            if dm_resolution == DmResolutionMode::AllowOpen {
                let opened = client
                    .conversations_open(session, &[user_id], false, false)
                    .await?;
                let channel_id = opened
                    .get("channel")
                    .and_then(|channel| channel.get("id"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        anyhow!("Slack did not return a channel ID from conversations.open")
                    })?
                    .to_string();
                self.conversations = None;
                return Ok(channel_id);
            }

            bail!(
                "no existing Slack DM matched `{raw}`; use `slackcli conversation open {raw}` to create or resume one explicitly"
            )
        }

        let normalized = normalize_conversation_reference(trimmed);
        if normalized.is_empty() {
            bail!("could not resolve Slack conversation `{raw}`")
        }

        let conversations = self.load_conversations(client, session).await?;
        let mut matches: Vec<(String, String)> = Vec::new();

        for conversation in conversations {
            if conversation_matches(conversation, &normalized)
                && let Some(id) = conversation.get("id").and_then(Value::as_str)
            {
                matches.push((id.to_string(), summarize_conversation(conversation)));
            }
        }

        matches.sort_by(|left, right| left.0.cmp(&right.0));
        matches.dedup_by(|left, right| left.0 == right.0);

        match matches.as_slice() {
            [(id, _summary)] => Ok(id.clone()),
            [] => bail!("could not resolve Slack conversation `{raw}`"),
            many => {
                let options = many
                    .iter()
                    .take(5)
                    .map(|(_, summary)| summary.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!("multiple Slack conversations matched `{raw}`: {options}")
            }
        }
    }

    async fn load_users(
        &mut self,
        client: &SlackClient,
        session: &RuntimeSession,
    ) -> Result<&[Value]> {
        if self.users.is_none() {
            let mut users = Vec::new();
            let mut cursor: Option<String> = None;

            loop {
                let page = client
                    .users_list(session, cursor.clone(), Some(200))
                    .await?;
                if let Some(members) = page.get("members").and_then(Value::as_array) {
                    users.extend(members.iter().cloned());
                }

                cursor = next_cursor(&page);
                if cursor.is_none() {
                    break;
                }
            }

            self.users = Some(users);
        }

        Ok(self.users.as_deref().unwrap_or(&[]))
    }

    async fn load_conversations(
        &mut self,
        client: &SlackClient,
        session: &RuntimeSession,
    ) -> Result<&[Value]> {
        if self.conversations.is_none() {
            let mut conversations = Vec::new();
            let mut cursor: Option<String> = None;

            loop {
                let mut query = vec![(
                    "types".to_string(),
                    "public_channel,private_channel,im,mpim".to_string(),
                )];
                if let Some(next_cursor) = &cursor {
                    query.push(("cursor".to_string(), next_cursor.clone()));
                }
                query.push(("limit".to_string(), "200".to_string()));

                let page = client.users_conversations(session, query).await?;
                if let Some(channels) = page.get("channels").and_then(Value::as_array) {
                    conversations.extend(channels.iter().cloned());
                }

                cursor = next_cursor(&page);
                if cursor.is_none() {
                    break;
                }
            }

            self.conversations = Some(conversations);
        }

        Ok(self.conversations.as_deref().unwrap_or(&[]))
    }

    async fn find_existing_im_with_user(
        &mut self,
        client: &SlackClient,
        session: &RuntimeSession,
        user_id: &str,
    ) -> Result<Option<String>> {
        let conversations = self.load_conversations(client, session).await?;
        Ok(existing_im_id(conversations, user_id))
    }
}

fn next_cursor(page: &Value) -> Option<String> {
    page.get("response_metadata")
        .and_then(|value| value.get("next_cursor"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|cursor| !cursor.is_empty())
        .map(str::to_string)
}

fn looks_like_user_id(raw: &str) -> bool {
    matches!(raw.chars().next(), Some('U' | 'W')) && raw.len() >= 2
}

fn looks_like_conversation_id(raw: &str) -> bool {
    matches!(raw.chars().next(), Some('C' | 'D' | 'G')) && raw.len() >= 2
}

fn normalize_user_reference(raw: &str) -> String {
    raw.trim()
        .trim_start_matches('@')
        .trim()
        .to_ascii_lowercase()
}

fn normalize_conversation_reference(raw: &str) -> String {
    raw.trim()
        .trim_start_matches('#')
        .trim()
        .to_ascii_lowercase()
}

fn user_matches(user: &Value, needle: &str) -> bool {
    user_candidate_strings(user)
        .into_iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(needle))
}

fn conversation_matches(conversation: &Value, needle: &str) -> bool {
    conversation
        .get("name")
        .and_then(Value::as_str)
        .map(|value| value.eq_ignore_ascii_case(needle))
        .unwrap_or(false)
}

fn existing_im_id(conversations: &[Value], user_id: &str) -> Option<String> {
    conversations.iter().find_map(|conversation| {
        if conversation
            .get("is_im")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && conversation.get("user").and_then(Value::as_str) == Some(user_id)
        {
            conversation
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
        } else {
            None
        }
    })
}

fn user_candidate_strings(user: &Value) -> Vec<String> {
    let mut values = Vec::new();

    push_string(&mut values, user.get("name"));
    push_string(&mut values, user.get("real_name"));

    if let Some(profile) = user.get("profile") {
        push_string(&mut values, profile.get("display_name"));
        push_string(&mut values, profile.get("display_name_normalized"));
        push_string(&mut values, profile.get("real_name"));
        push_string(&mut values, profile.get("real_name_normalized"));
        push_string(&mut values, profile.get("email"));
    }

    values
        .into_iter()
        .map(|value| value.trim().trim_start_matches('@').to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

fn summarize_user(user: &Value) -> String {
    let id = user
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("unknown-id");
    let profile = user.get("profile");
    let preferred_name = profile
        .and_then(|profile| profile.get("display_name"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .or_else(|| user.get("name").and_then(Value::as_str))
        .unwrap_or("unknown-user");
    let email = profile
        .and_then(|profile| profile.get("email"))
        .and_then(Value::as_str);

    if let Some(email) = email {
        format!("{preferred_name} ({id}, {email})")
    } else {
        format!("{preferred_name} ({id})")
    }
}

fn summarize_conversation(conversation: &Value) -> String {
    let id = conversation
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("unknown-id");
    let name = conversation
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("unknown-conversation");
    format!("#{name} ({id})")
}

fn push_string(values: &mut Vec<String>, value: Option<&Value>) {
    if let Some(value) = value.and_then(Value::as_str) {
        values.push(value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn matches_user_names_and_emails() {
        let user = json!({
            "id": "U123",
            "name": "alice",
            "profile": {
                "display_name": "Alice W",
                "real_name": "Alice Wonder",
                "email": "alice@example.com"
            }
        });

        assert!(user_matches(&user, "alice"));
        assert!(user_matches(&user, "alice w"));
        assert!(user_matches(&user, "alice@example.com"));
        assert!(!user_matches(&user, "bob"));
    }

    #[test]
    fn matches_conversation_names_without_hash() {
        let conversation = json!({
            "id": "C123",
            "name": "general"
        });

        assert!(conversation_matches(&conversation, "general"));
        assert!(!conversation_matches(&conversation, "#general"));
        assert_eq!(normalize_conversation_reference("#general"), "general");
    }

    #[test]
    fn detects_id_shapes() {
        assert!(looks_like_user_id("U123"));
        assert!(looks_like_conversation_id("C123"));
        assert!(looks_like_conversation_id("D123"));
        assert!(!looks_like_conversation_id("general"));
    }

    #[test]
    fn finds_existing_im_ids_without_opening() {
        let conversations = vec![
            json!({
                "id": "C123",
                "name": "general",
                "is_im": false,
            }),
            json!({
                "id": "D456",
                "is_im": true,
                "user": "U999",
            }),
        ];

        assert_eq!(existing_im_id(&conversations, "U999"), Some("D456".into()));
        assert_eq!(existing_im_id(&conversations, "U404"), None);
    }
}
