use crate::clients::slack::{Message, SlackClient};
use crate::{Event, IntegrationEvent, Node, NodeKind, NodeMetadata, NodeStatus, RelationType, extract_context, current_timestamp};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

pub enum SlackCommand {
    AddChannel(String),
    RemoveChannel(String),
    ResolveEphemeralNodes,
    Shutdown,
}

pub struct SlackActor {
    client: SlackClient,
    workspace_id: String,
    channels: HashSet<String>,
    poll_interval: Duration,
    commands: UnboundedReceiver<SlackCommand>,
    events: UnboundedSender<IntegrationEvent>,
    nodes: UnboundedSender<Node>,
    // CHANGED: Track latest timestamp per channel instead of cursor
    last_ts: HashMap<String, String>,
    // NEW: Track seen message IDs to prevent duplicates
    seen_messages: HashSet<String>,
}

impl SlackActor {
    pub fn new(
        token: String,
        workspace_id: String,
        poll_interval: Duration,
        commands: UnboundedReceiver<SlackCommand>,
        events: UnboundedSender<IntegrationEvent>,
        nodes: UnboundedSender<Node>,
    ) -> Self {
        Self {
            client: SlackClient::new(token),
            workspace_id,
            channels: HashSet::new(),
            poll_interval,
            commands,
            events,
            nodes,
            last_ts: HashMap::new(),
            seen_messages: HashSet::new(),
        }
    }

    pub async fn run(mut self) {
        let mut interval = tokio::time::interval(self.poll_interval);

        // Log startup
        tracing::info!("🚀 SlackActor started for workspace {}", self.workspace_id);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(e) = self.poll_channels().await {
                        tracing::error!("Poll error: {:?}", e);
                    }
                }
                Some(cmd) = self.commands.recv() => {
                    match cmd {
                        SlackCommand::AddChannel(id) => {
                            tracing::info!("➕ Adding channel {} to workspace {}", id, self.workspace_id);
                            self.channels.insert(id);
                        }
                        SlackCommand::RemoveChannel(id) => {
                            tracing::info!("➖ Removing channel {}", id);
                            self.channels.remove(&id);
                            self.last_ts.remove(&id);
                        }
                        SlackCommand::ResolveEphemeralNodes => {
                            // Handle if needed
                        }
                        SlackCommand::Shutdown => {
                            tracing::info!("🛑 SlackActor shutting down");
                            break;
                        }
                    }
                }
            }
        }
    }

    async fn poll_channels(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.channels.is_empty() {
            tracing::debug!("No channels configured, skipping poll");
            return Ok(());
        }

        tracing::debug!("🔄 Polling {} channels", self.channels.len());

        for channel_id in self.channels.clone() {
            // Use oldest= parameter to only get messages newer than last seen
            let oldest = self.last_ts.get(&channel_id).map(|s| s.as_str());

            tracing::debug!("  Channel {}: oldest={:?}", channel_id, oldest);

            match self.client.get_messages_since(&channel_id, oldest, Some("100")).await {
                Ok(history) => {
                    let msg_count = history.messages.len();
                    tracing::info!("📬 Got {} messages from channel {}", msg_count, channel_id);

                    let mut newest_ts: Option<String> = None;
                    let mut new_count = 0;

                    for message in &history.messages {
                        // Skip if already seen
                        let msg_key = format!("{}:{}", channel_id, message.ts);
                        if self.seen_messages.contains(&msg_key) {
                            continue;
                        }
                        self.seen_messages.insert(msg_key);
                        new_count += 1;

                        // Track newest timestamp
                        if newest_ts.as_ref().map_or(true, |ts| message.ts > *ts) {
                            newest_ts = Some(message.ts.clone());
                        }

                        // Create and send node
                        let node = self.create_node_from_message(&channel_id, message);
                        if let Err(e) = self.nodes.send(node.clone()) {
                            tracing::error!("Failed to send node: {}", e);
                        } else {
                            tracing::debug!("📤 Sent node for message {}", message.ts);
                        }

                        // Extract and send events
                        let events = self.extract_events_from_message(&channel_id, message, &node);
                        for event in events {
                            if let Err(e) = self.events.send(event) {
                                tracing::error!("Failed to send event: {}", e);
                            }
                        }
                    }

                    // Update last seen timestamp
                    if let Some(ts) = newest_ts {
                        self.last_ts.insert(channel_id.clone(), ts);
                    }

                    if new_count > 0 {
                        tracing::info!("✅ Processed {} new messages from {}", new_count, channel_id);
                    }
                }
                Err(e) => {
                    tracing::error!("❌ Failed to fetch messages for {}: {}", channel_id, e);
                }
            }
        }

        // Trim seen_messages if it gets too large (keep last 10k)
        if self.seen_messages.len() > 10000 {
            // Just clear it - messages will be de-duped by timestamp anyway
            self.seen_messages.clear();
            tracing::debug!("Cleared seen_messages cache");
        }

        Ok(())
    }

    fn create_node_from_message(&self, channel_id: &str, message: &Message) -> Node {
        let node_id = format!("slack:{}:{}", channel_id, message.ts);
        let node_id = blake3::hash(node_id.as_bytes()).to_hex().to_string();

        Node {
            id: node_id,
            workspace_id: self.workspace_id.clone(),
            external_id: message.ts.clone(),
            name: format!("Message in {}", channel_id),
            kind: NodeKind::SlackMessage,
            content: message.text.clone(),
            metadata: NodeMetadata {
                author: message.user.clone(),
                url: format!("https://slack.com/archives/{}/p{}", channel_id, message.ts.replace(".", "")),
                title: None,
                status: None,
                mentioned_by: vec![],
                mention_count: 0,
            },
            status: NodeStatus::Real,
            ts: message.ts.parse::<f64>().unwrap_or(0.0) as u64,
            first_seen: current_timestamp(),
            last_updated: current_timestamp(),
        }
    }

    fn extract_events_from_message(&self, channel_id: &str, message: &Message, node: &Node) -> Vec<IntegrationEvent> {
        let mut events = Vec::new();
        let text = &message.text;

        // Extract JIRA references
        let jira_regex = regex::Regex::new(r"\b([A-Z]{2,}-\d+)\b").unwrap();
        for capture in jira_regex.captures_iter(text) {
            if let Some(jira_id) = capture.get(1) {
                let event_id = blake3::hash(
                    format!("{}:jira:{}", node.id, jira_id.as_str()).as_bytes()
                ).to_hex().to_string();

                events.push(IntegrationEvent {
                    base: Event {
                        id: event_id,
                        payload: serde_json::json!({
                            "type": "jira_mention",
                            "jira_id": jira_id.as_str(),
                            "channel": channel_id,
                            "context": extract_context(text, capture.get(0).unwrap().start(), 50),
                        }).to_string(),
                        source: node.id.clone(),
                        ts: node.ts,
                    },
                    workspace_id: self.workspace_id.clone(),
                    relation: RelationType::Mentions,
                    confidence: 0.95,
                    discovered_at: current_timestamp(),
                });
            }
        }

        events
    }
}