use crate::clients::slack::{Message, SlackClient};
use crate::{Event, IntegrationEvent, Node, NodeKind, NodeMetadata, NodeStatus, RelationType, extract_context, current_timestamp};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

// Define SlackCommand enum
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
    last_fetch: HashMap<String, Option<String>>, // channel -> cursor
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
            last_fetch: HashMap::new(),
        }
    }

    pub async fn run(mut self) {
        let mut interval = tokio::time::interval(self.poll_interval);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(e) = self.poll_channels().await {
                        eprintln!("Poll error: {:?}", e);
                    }
                }
                Some(cmd) = self.commands.recv() => {
                    match cmd {
                        SlackCommand::AddChannel(id) => {
                            self.channels.insert(id);
                        }
                        SlackCommand::RemoveChannel(id) => {
                            self.channels.remove(&id);
                        }
                        SlackCommand::ResolveEphemeralNodes => {
                            // Handle if needed
                        }
                        SlackCommand::Shutdown => break,
                    }
                }
            }
        }
    }

    async fn poll_channels(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for channel_id in self.channels.clone() {
            let cursor = self.last_fetch.get(&channel_id).and_then(|c| c.as_deref());

            match self.client.get_messages(&channel_id, cursor, Some("10")).await {
                Ok(history) => {
                    // Process messages
                    for message in &history.messages {
                        let node = self.create_node_from_message(&channel_id, message);
                        let _ = self.nodes.send(node.clone());

                        // Extract events from message
                        let events = self.extract_events_from_message(&channel_id, message, &node);
                        for event in events {
                            let _ = self.events.send(event);
                        }
                    }

                    // Update cursor
                    if let Some(metadata) = history.response_metadata {
                        self.last_fetch.insert(channel_id.clone(), metadata.next_cursor);
                    }
                }
                Err(e) => {
                    eprintln!("Failed to fetch messages for {}: {}", channel_id, e);
                }
            }
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