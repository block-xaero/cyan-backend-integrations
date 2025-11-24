use crate::clients::googledocs::{GoogleDocsClient, GoogleDoc};
use crate::{Node, NodeKind, NodeStatus, NodeMetadata, Event, IntegrationEvent, RelationType, NodeCache, current_timestamp};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

pub struct GoogleDocsActor {
    workspace_id: String,
    document_ids: HashSet<String>,  // Document IDs to monitor
    poll_interval: Duration,
    client: Arc<GoogleDocsClient>,
    commands: UnboundedReceiver<GoogleDocsCommand>,
    events: UnboundedSender<IntegrationEvent>,
    nodes: UnboundedSender<Node>,
    node_cache: Arc<NodeCache>,
}

pub enum GoogleDocsCommand {
    AddDocument(String),
    RemoveDocument(String),
    ResolveEphemeralNodes,
    Shutdown,
}

impl GoogleDocsActor {
    pub fn new(
        access_token: String,
        workspace_id: String,
        poll_interval: Duration,
        commands: UnboundedReceiver<GoogleDocsCommand>,
        events: UnboundedSender<IntegrationEvent>,
        nodes: UnboundedSender<Node>,
        node_cache: Arc<NodeCache>,
    ) -> Self {
        Self {
            workspace_id,
            document_ids: HashSet::new(),
            poll_interval,
            client: Arc::new(GoogleDocsClient::new(access_token)),
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
                    if let Err(e) = self.sync_documents().await {
                        eprintln!("Google Docs sync error: {:?}", e);
                    }

                    if let Err(e) = self.resolve_ephemeral_nodes().await {
                        eprintln!("Failed to resolve ephemeral nodes: {:?}", e);
                    }
                }
                Some(cmd) = self.commands.recv() => {
                    match cmd {
                        GoogleDocsCommand::AddDocument(id) => {
                            self.document_ids.insert(id);
                        }
                        GoogleDocsCommand::RemoveDocument(id) => {
                            self.document_ids.remove(&id);
                        }
                        GoogleDocsCommand::ResolveEphemeralNodes => {
                            let _ = self.resolve_ephemeral_nodes().await;
                        }
                        GoogleDocsCommand::Shutdown => break,
                    }
                }
            }
        }
    }

    async fn sync_documents(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for doc_id in &self.document_ids {
            match self.client.get_document(doc_id).await {
                Ok(doc) => {
                    let node = self.create_node_from_doc(&doc);

                    let resolved_node = self.node_cache.resolve_ephemeral(
                        &doc.document_id,
                        NodeKind::GoogleDoc,
                        node.clone()
                    ).await;

                    self.nodes.send(resolved_node)?;

                    let events = self.extract_events_from_doc(&doc, &node);
                    for event in events {
                        self.events.send(event)?;
                    }
                }
                Err(e) => eprintln!("Failed to fetch doc {}: {}", doc_id, e),
            }
        }

        Ok(())
    }

    async fn resolve_ephemeral_nodes(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let ephemeral = self.node_cache.get_ephemeral_nodes(NodeKind::GoogleDoc).await;

        for ghost_node in ephemeral {
            match self.client.get_document(&ghost_node.external_id).await {
                Ok(doc) => {
                    let real_node = self.create_node_from_doc(&doc);
                    let resolved = self.node_cache.resolve_ephemeral(
                        &ghost_node.external_id,
                        NodeKind::GoogleDoc,
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

    fn create_node_from_doc(&self, doc: &GoogleDoc) -> Node {
        let node_id = NodeCache::generate_node_id(&NodeKind::GoogleDoc, &doc.document_id);

        let content = self.extract_text_content(doc);

        Node {
            id: node_id,
            workspace_id: self.workspace_id.clone(),
            external_id: doc.document_id.clone(),
            name: doc.title.clone(),
            kind: NodeKind::GoogleDoc,
            content: content.chars().take(1000).collect(),
            metadata: NodeMetadata {
                author: String::new(), // Would need separate API call to get author
                url: format!("https://docs.google.com/document/d/{}/edit", doc.document_id),
                title: Some(doc.title.clone()),
                status: None,
                mentioned_by: vec![],
                mention_count: 0,
            },
            status: NodeStatus::Real,
            ts: current_timestamp(), // Would need Drive API for real modified time
            first_seen: current_timestamp(),
            last_updated: current_timestamp(),
        }
    }

    fn extract_text_content(&self, doc: &GoogleDoc) -> String {
        let mut text = String::new();

        if let Some(body) = &doc.body {
            for element in &body.content {
                if let Some(paragraph) = &element.paragraph {
                    for p_element in &paragraph.elements {
                        if let Some(text_run) = &p_element.text_run {
                            text.push_str(&text_run.content);
                        }
                    }
                }
            }
        }

        text
    }

    fn extract_events_from_doc(&self, doc: &GoogleDoc, node: &Node) -> Vec<IntegrationEvent> {
        let mut events = Vec::new();
        let content = self.extract_text_content(doc);

        // Look for JIRA references
        let jira_regex = regex::Regex::new(r"\b([A-Z]{2,}-\d+)\b").unwrap();
        for capture in jira_regex.captures_iter(&content) {
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
                            "doc_id": doc.document_id,
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

        events
    }
}