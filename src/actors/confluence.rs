use crate::clients::confluence::{ConfluenceClient, ConfluencePage};
use crate::{Node, NodeKind, NodeStatus, NodeMetadata, Event, IntegrationEvent, RelationType, NodeCache, current_timestamp};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

pub struct ConfluenceActor {
    workspace_id: String,
    spaces: HashSet<String>,  // Space keys to monitor
    poll_interval: Duration,
    client: Arc<ConfluenceClient>,
    commands: UnboundedReceiver<ConfluenceCommand>,
    events: UnboundedSender<IntegrationEvent>,
    nodes: UnboundedSender<Node>,
    node_cache: Arc<NodeCache>,
}

pub enum ConfluenceCommand {
    AddSpace(String),
    RemoveSpace(String),
    ResolveEphemeralNodes,
    Shutdown,
}

impl ConfluenceActor {
    pub fn new(
        domain: String,
        email: String,
        api_token: String,
        workspace_id: String,
        poll_interval: Duration,
        commands: UnboundedReceiver<ConfluenceCommand>,
        events: UnboundedSender<IntegrationEvent>,
        nodes: UnboundedSender<Node>,
        node_cache: Arc<NodeCache>,
    ) -> Self {
        Self {
            workspace_id,
            spaces: HashSet::new(),
            poll_interval,
            client: Arc::new(ConfluenceClient::new(domain, email, api_token)),
            commands,
            events,
            nodes,
            node_cache,
        }
    }

    pub async fn run(mut self) {
        let mut interval = tokio::time::interval(self.poll_interval);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(e) = self.sync_spaces().await {
                        eprintln!("Confluence sync error: {:?}", e);
                    }

                    if let Err(e) = self.resolve_ephemeral_nodes().await {
                        eprintln!("Failed to resolve ephemeral nodes: {:?}", e);
                    }
                }
                Some(cmd) = self.commands.recv() => {
                    match cmd {
                        ConfluenceCommand::AddSpace(key) => {
                            self.spaces.insert(key);
                        }
                        ConfluenceCommand::RemoveSpace(key) => {
                            self.spaces.remove(&key);
                        }
                        ConfluenceCommand::ResolveEphemeralNodes => {
                            let _ = self.resolve_ephemeral_nodes().await;
                        }
                        ConfluenceCommand::Shutdown => break,
                    }
                }
            }
        }
    }

    async fn sync_spaces(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for space_key in &self.spaces {
            let pages = self.client.get_space_pages(space_key).await?;

            for page in pages {
                let node = self.create_node_from_page(&page);

                let resolved_node = self.node_cache.resolve_ephemeral(
                    &page.id,
                    NodeKind::ConfluencePage,
                    node.clone()
                ).await;

                self.nodes.send(resolved_node)?;

                let events = self.extract_events_from_page(&page, &node);
                for event in events {
                    self.events.send(event)?;
                }
            }
        }

        Ok(())
    }

    async fn resolve_ephemeral_nodes(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let ephemeral = self.node_cache.get_ephemeral_nodes(NodeKind::ConfluencePage).await;

        for ghost_node in ephemeral {
            match self.client.get_page(&ghost_node.external_id).await {
                Ok(page) => {
                    let real_node = self.create_node_from_page(&page);
                    let resolved = self.node_cache.resolve_ephemeral(
                        &ghost_node.external_id,
                        NodeKind::ConfluencePage,
                        real_node
                    ).await;

                    self.nodes.send(resolved)?;
                }
                Err(_) => {
                    let mut not_found = ghost_node.clone();
                    not_found.status = NodeStatus::NotFound;
                    self.nodes.send(not_found)?;
                }
            }
        }

        Ok(())
    }

    fn create_node_from_page(&self, page: &ConfluencePage) -> Node {
        let node_id = NodeCache::generate_node_id(&NodeKind::ConfluencePage, &page.id);

        let content = page.body.as_ref()
            .and_then(|b| Some(b.storage.value.clone()))
            .unwrap_or_default();

        Node {
            id: node_id,
            workspace_id: self.workspace_id.clone(),
            external_id: page.id.clone(),
            name: page.title.clone(),
            kind: NodeKind::ConfluencePage,
            content: content.chars().take(1000).collect(), // First 1000 chars
            metadata: NodeMetadata {
                author: page.version.by.as_ref()
                    .map(|u| u.display_name.clone())
                    .unwrap_or_default(),
                url: format!("https://confluence.example.com{}", page.links.webui),
                title: Some(page.title.clone()),
                status: None,
                mentioned_by: vec![],
                mention_count: 0,
            },
            status: NodeStatus::Real,
            ts: chrono::DateTime::parse_from_rfc3339(&page.version.when)
                .map(|dt| dt.timestamp() as u64)
                .unwrap_or(0),
            first_seen: current_timestamp(),
            last_updated: current_timestamp(),
        }
    }

    fn extract_events_from_page(&self, page: &ConfluencePage, node: &Node) -> Vec<IntegrationEvent> {
        let mut events = Vec::new();

        if let Some(body) = &page.body {
            let content = &body.storage.value;

            // Look for JIRA references
            let jira_regex = regex::Regex::new(r"\b([A-Z]{2,}-\d+)\b").unwrap();
            for capture in jira_regex.captures_iter(content) {
                if let Some(jira_id) = capture.get(1) {
                    let event_id = blake3::hash(
                        format!("{}:jira:{}", node.id, jira_id.as_str()).as_bytes()
                    ).to_hex().to_string();

                    events.push(IntegrationEvent {
                        base: Event {
                            id: event_id,
                            payload: serde_json::json!({
                                "type": "jira_reference",
                                "jira_id": jira_id.as_str(),
                                "page_id": page.id,
                            }).to_string(),
                            source: node.id.clone(),
                            ts: node.ts,
                        },
                        workspace_id: self.workspace_id.clone(),
                        relation: RelationType::References,
                        confidence: 0.9,
                        discovered_at: current_timestamp(),
                    });
                }
            }
        }

        events
    }
}