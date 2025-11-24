use crate::clients::jira::{JiraClient, JiraIssue};
use crate::{Node, NodeKind, NodeStatus, NodeMetadata, Event, IntegrationEvent, RelationType, NodeCache, current_timestamp};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

pub struct JiraActor {
    workspace_id: String,
    projects: HashSet<String>,  // Project keys to monitor
    poll_interval: Duration,
    client: Arc<JiraClient>,
    commands: UnboundedReceiver<JiraCommand>,
    events: UnboundedSender<IntegrationEvent>,
    nodes: UnboundedSender<Node>,
    node_cache: Arc<NodeCache>,
    last_sync: HashMap<String, String>, // project -> last update time
}

pub enum JiraCommand {
    AddProject(String),
    RemoveProject(String),
    ResolveEphemeralNodes,
    Shutdown,
}

impl JiraActor {
    pub fn new(
        domain: String,
        email: String,
        api_token: String,
        workspace_id: String,
        poll_interval: Duration,
        commands: UnboundedReceiver<JiraCommand>,
        events: UnboundedSender<IntegrationEvent>,
        nodes: UnboundedSender<Node>,
        node_cache: Arc<NodeCache>,
    ) -> Self {
        Self {
            workspace_id,
            projects: HashSet::new(),
            poll_interval,
            client: Arc::new(JiraClient::new(domain, email, api_token)),
            commands,
            events,
            nodes,
            node_cache,
            last_sync: HashMap::new(),
        }
    }

    pub async fn run(mut self) {
        let mut interval = tokio::time::interval(self.poll_interval);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(e) = self.sync_projects().await {
                        eprintln!("JIRA sync error: {:?}", e);
                    }

                    // Also resolve ephemeral nodes
                    if let Err(e) = self.resolve_ephemeral_nodes().await {
                        eprintln!("Failed to resolve ephemeral nodes: {:?}", e);
                    }
                }
                Some(cmd) = self.commands.recv() => {
                    match cmd {
                        JiraCommand::AddProject(key) => {
                            self.projects.insert(key);
                        }
                        JiraCommand::RemoveProject(key) => {
                            self.projects.remove(&key);
                        }
                        JiraCommand::ResolveEphemeralNodes => {
                            let _ = self.resolve_ephemeral_nodes().await;
                        }
                        JiraCommand::Shutdown => break,
                    }
                }
            }
        }
    }

    async fn sync_projects(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for project_key in &self.projects {
            // Build JQL query for updated issues
            let jql = format!("project = {} ORDER BY updated DESC", project_key);

            let issues = self.client.search_issues(&jql).await?;

            for issue in issues {
                let node = self.create_node_from_issue(&issue);

                // Check if this was an ephemeral node and resolve it
                let resolved_node = self.node_cache.resolve_ephemeral(
                    &issue.key,
                    NodeKind::JiraTicket,
                    node.clone()
                ).await;

                self.nodes.send(resolved_node)?;

                // Extract events (links to other systems mentioned in description/comments)
                let events = self.extract_events_from_issue(&issue, &node);
                for event in events {
                    self.events.send(event)?;
                }
            }
        }

        Ok(())
    }

    async fn resolve_ephemeral_nodes(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Get all ephemeral JIRA nodes
        let ephemeral = self.node_cache.get_ephemeral_nodes(NodeKind::JiraTicket).await;

        for ghost_node in ephemeral {
            // Try to fetch real data
            match self.client.get_issue(&ghost_node.external_id).await {
                Ok(issue) => {
                    let real_node = self.create_node_from_issue(&issue);
                    let resolved = self.node_cache.resolve_ephemeral(
                        &ghost_node.external_id,
                        NodeKind::JiraTicket,
                        real_node
                    ).await;

                    self.nodes.send(resolved)?;
                }
                Err(_) => {
                    // Mark as not found
                    let mut not_found = ghost_node.clone();
                    not_found.status = NodeStatus::NotFound;
                    self.nodes.send(not_found)?;
                }
            }
        }

        Ok(())
    }

    fn create_node_from_issue(&self, issue: &JiraIssue) -> Node {
        let node_id = NodeCache::generate_node_id(&NodeKind::JiraTicket, &issue.key);

        Node {
            id: node_id,
            workspace_id: self.workspace_id.clone(),
            external_id: issue.key.clone(),
            name: format!("{}: {}", issue.key, issue.fields.summary),
            kind: NodeKind::JiraTicket,
            content: serde_json::json!({
                "summary": issue.fields.summary,
                "description": issue.fields.description,
                "status": issue.fields.status.name,
                "assignee": issue.fields.assignee.as_ref().map(|u| u.display_name.clone()),
            }).to_string(),
            metadata: NodeMetadata {
                author: issue.fields.reporter
                    .as_ref()
                    .map(|r| r.display_name.clone())
                    .unwrap_or_default(),
                url: format!("https://jira.example.com/browse/{}", issue.key),
                title: Some(issue.fields.summary.clone()),
                status: Some(issue.fields.status.name.clone()),
                mentioned_by: vec![],
                mention_count: 0,
            },
            status: NodeStatus::Real,
            ts: chrono::DateTime::parse_from_rfc3339(&issue.fields.created)
                .map(|dt| dt.timestamp() as u64)
                .unwrap_or(0),
            first_seen: current_timestamp(),
            last_updated: current_timestamp(),
        }
    }

    fn extract_events_from_issue(&self, issue: &JiraIssue, node: &Node) -> Vec<IntegrationEvent> {
        let mut events = Vec::new();

        // Check description for GitHub references
        if let Some(description) = &issue.fields.description {
            // Extract GitHub PR references
            let pr_regex = regex::Regex::new(r"(?i)PR\s*#?(\d+)").unwrap();
            for capture in pr_regex.captures_iter(description) {
                if let Some(pr_num) = capture.get(1) {
                    let event_id = blake3::hash(
                        format!("{}:github:{}", node.id, pr_num.as_str()).as_bytes()
                    ).to_hex().to_string();

                    events.push(IntegrationEvent {
                        base: Event {
                            id: event_id,
                            payload: serde_json::json!({
                                "type": "github_pr_reference",
                                "pr_number": pr_num.as_str(),
                                "jira_key": issue.key,
                            }).to_string(),
                            source: node.id.clone(),
                            ts: node.ts,
                        },
                        workspace_id: self.workspace_id.clone(),
                        relation: RelationType::Implements,
                        confidence: 0.85,
                        discovered_at: current_timestamp(),
                    });
                }
            }
        }

        events
    }
}