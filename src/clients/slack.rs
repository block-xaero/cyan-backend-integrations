use reqwest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::error::Error;
use std::fmt;

// Error handling
#[derive(Debug)]
pub enum SlackError {
    Network(reqwest::Error),
    Parse(serde_json::Error),
    Api(String),
}

impl fmt::Display for SlackError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            SlackError::Network(e) => write!(f, "Network error: {}", e),
            SlackError::Parse(e) => write!(f, "Parse error: {}", e),
            SlackError::Api(msg) => write!(f, "Slack API error: {}", msg),
        }
    }
}

impl Error for SlackError {}

impl From<reqwest::Error> for SlackError {
    fn from(e: reqwest::Error) -> Self {
        SlackError::Network(e)
    }
}

impl From<serde_json::Error> for SlackError {
    fn from(e: serde_json::Error) -> Self {
        SlackError::Parse(e)
    }
}

// Response types
#[derive(Debug, Deserialize)]
pub struct SlackResponse<T> {
    pub ok: bool,
    pub error: Option<String>,
    #[serde(flatten)]
    pub data: Option<T>,
}

#[derive(Debug, Deserialize)]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub is_private: bool,
    pub is_channel: bool,
    pub is_group: bool,
    pub is_im: bool,
    pub created: u64,
    pub creator: String,
    pub is_archived: bool,
    pub is_general: bool,
    pub name_normalized: String,
    pub is_shared: bool,
    pub is_member: bool,
    pub topic: Option<ChannelTopic>,
    pub purpose: Option<ChannelPurpose>,
    pub num_members: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct ChannelTopic {
    pub value: String,
    pub creator: String,
    pub last_set: u64,
}

#[derive(Debug, Deserialize)]
pub struct ChannelPurpose {
    pub value: String,
    pub creator: String,
    pub last_set: u64,
}

#[derive(Debug, Deserialize)]
pub struct ChannelListResponse {
    pub ok: bool,
    pub channels: Vec<Channel>,
    pub response_metadata: Option<ResponseMetadata>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Message {
    pub text: String,
    pub user: String,
    pub ts: String,
    pub thread_ts: Option<String>,
    pub reply_count: Option<u32>,
    pub reply_users_count: Option<u32>,
    pub latest_reply: Option<String>,
    pub subscribed: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct MessageHistory {
    pub ok: bool,
    pub messages: Vec<Message>,
    pub has_more: bool,
    pub response_metadata: Option<ResponseMetadata>,
}

#[derive(Debug, Deserialize)]
pub struct ThreadMessages {
    pub ok: bool,
    pub messages: Vec<Message>,
}

#[derive(Debug, Deserialize)]
pub struct ResponseMetadata {
    pub next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PostMessageResponse {
    pub ok: bool,
    pub channel: String,
    pub ts: String,
    pub message: Option<Message>,
}

#[derive(Debug, Deserialize)]
pub struct UserInfo {
    pub id: String,
    pub name: String,
    pub real_name: Option<String>,
    pub display_name: Option<String>,
    pub is_bot: bool,
}

#[derive(Debug, Deserialize)]
pub struct UserResponse {
    pub ok: bool,
    pub user: UserInfo,
}

#[derive(Debug)]
pub struct SlackClient {
    client: reqwest::Client,
    token: String,
    base_url: String,
}

impl SlackClient {
    pub fn new(token: String) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap(),
            token,
            base_url: "https://slack.com/api".to_string(),
        }
    }

    async fn make_get_request<T: for<'de> Deserialize<'de>>(
        &self,
        endpoint: &str,
        params: Option<HashMap<&str, &str>>,
    ) -> Result<T, SlackError> {
        let url = format!("{}/{}", self.base_url, endpoint);
        let mut req = self.client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token));

        if let Some(p) = params {
            req = req.query(&p);
        }

        let response = req.send().await?;
        let data: T = response.json().await?;
        Ok(data)
    }

    async fn make_post_request<T: for<'de> Deserialize<'de>>(
        &self,
        endpoint: &str,
        body: serde_json::Value,
    ) -> Result<T, SlackError> {
        let url = format!("{}/{}", self.base_url, endpoint);

        let response = self.client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let data: T = response.json().await?;
        Ok(data)
    }

    // API Methods
    pub async fn test_auth(&self) -> Result<bool, SlackError> {
        #[derive(Deserialize)]
        struct AuthTest {
            ok: bool,
            url: Option<String>,
            team: Option<String>,
            user: Option<String>,
            team_id: Option<String>,
            user_id: Option<String>,
        }

        let response: AuthTest = self
            .make_get_request("auth.test", None)
            .await?;

        Ok(response.ok)
    }

    pub async fn list_channels(&self) -> Result<Vec<Channel>, SlackError> {
        let mut all_channels = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let mut params = HashMap::new();
            params.insert("types", "public_channel,private_channel");
            params.insert("limit", "100");

            if let Some(c) = cursor.as_ref() {
                params.insert("cursor", c);
            }

            let response: ChannelListResponse = self
                .make_get_request("conversations.list", Some(params))
                .await?;

            if !response.ok {
                return Err(SlackError::Api("Failed to list channels".to_string()));
            }

            all_channels.extend(response.channels);

            match response.response_metadata {
                Some(meta) if meta.next_cursor.is_some() => {
                    cursor = meta.next_cursor;
                }
                _ => break,
            }
        }

        Ok(all_channels)
    }

    pub async fn get_messages(
        &self,
        channel_id: &str,
        cursor: Option<&str>,
        limit: Option<&str>,
    ) -> Result<MessageHistory, SlackError> {
        let mut params = HashMap::new();
        params.insert("channel", channel_id);
        // FIXME: default limit needs to fix.
        params.insert("limit", limit.unwrap_or("100"));

        if let Some(c) = cursor {
            params.insert("cursor", c);
        }

        let response: MessageHistory = self
            .make_get_request("conversations.history", Some(params))
            .await?;

        if !response.ok {
            return Err(SlackError::Api(format!("Failed to get messages for channel {}", channel_id)));
        }

        Ok(response)
    }

    pub async fn get_all_messages(
        &self,
        channel_id: &str,
    ) -> Result<Vec<Message>, SlackError> {
        let mut all_messages = Vec::new();
        let mut cursor = None;

        loop {
            let history = self.get_messages(channel_id, cursor.as_deref(), None).await?;
            all_messages.extend(history.messages);

            if !history.has_more {
                break;
            }

            cursor = history.response_metadata.and_then(|m| m.next_cursor);
        }

        Ok(all_messages)
    }

    pub async fn get_thread(
        &self,
        channel_id: &str,
        thread_ts: &str,
    ) -> Result<Vec<Message>, SlackError> {
        let mut params = HashMap::new();
        params.insert("channel", channel_id);
        params.insert("ts", thread_ts);

        let response: ThreadMessages = self
            .make_get_request("conversations.replies", Some(params))
            .await?;

        if !response.ok {
            return Err(SlackError::Api(format!("Failed to get thread {}", thread_ts)));
        }

        Ok(response.messages)
    }

    pub async fn post_message(
        &self,
        channel_id: &str,
        text: &str,
        thread_ts: Option<&str>,
    ) -> Result<PostMessageResponse, SlackError> {
        let mut body = serde_json::json!({
            "channel": channel_id,
            "text": text
        });

        if let Some(ts) = thread_ts {
            body["thread_ts"] = serde_json::Value::String(ts.to_string());
        }

        let response: PostMessageResponse = self
            .make_post_request("chat.postMessage", body)
            .await?;

        if !response.ok {
            return Err(SlackError::Api(format!("Failed to post message to {}", channel_id)));
        }

        Ok(response)
    }

    pub async fn get_user_info(&self, user_id: &str) -> Result<UserInfo, SlackError> {
        let mut params = HashMap::new();
        params.insert("user", user_id);

        let response: UserResponse = self
            .make_get_request("users.info", Some(params))
            .await?;

        if !response.ok {
            return Err(SlackError::Api(format!("Failed to get user info for {}", user_id)));
        }

        Ok(response.user)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_client_creation() {
        let client = SlackClient::new("test-token".to_string());
        assert_eq!(client.token, "test-token");
    }
}