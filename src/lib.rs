mod clients;
mod actors;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::RwLock;
use blake3;

// Import actor types and their commands
use actors::{
    slack::{SlackActor, SlackCommand},
    jira::{JiraActor, JiraCommand},
    github::{GitHubActor, GitHubCommand},
    confluence::{ConfluenceActor, ConfluenceCommand},
    googledocs::{GoogleDocsActor, GoogleDocsCommand},
};

// Event payload needs structure, not just String
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventPayload {
    Message {
        content: String,
        author: String,
        thread_id: Option<String>,
        mentions: Vec<String>,
    },
    Issue {
        title: String,
        status: String,
        assignee: Option<String>,
        priority: String,
    },
    Document {
        title: String,
        excerpt: String, // First 500 chars
        last_edited: u64,
    },
    CodeChange {
        diff_summary: String,
        files_changed: Vec<String>,
        commit_msg: String,
    },
}

// XaeroFlux-compatible base event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,           // Unique event ID (blake3 hash)
    pub payload: String,      // Application-specific data
    pub source: String,       // Node ID that created this event
    pub ts: u64,              // Unix timestamp
}

// Integration-specific event with more details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationEvent {
    pub base: Event,          // XaeroFlux compatible event
    pub workspace_id: String,
    pub relation: RelationType,
    pub confidence: f32,
    pub discovered_at: u64,
}

/// Node status - critical for ephemeral node tracking
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NodeStatus {
    Real,           // Full data from source integration
    Ephemeral,      // Mentioned but not fetched yet
    Resolved,       // Was ephemeral, now has real data
    NotFound,       // Tried to fetch but doesn't exist
    Error(String),  // Fetch attempted but failed
}

/// A node is an integration point
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String,           // Blake3 hash - STABLE across ephemeral→real
    pub workspace_id: String,
    pub external_id: String,  // PROJ-123, PR#45, etc.
    pub name: String,
    pub kind: NodeKind,
    pub content: String,      // Empty for ephemeral nodes
    pub metadata: NodeMetadata,
    pub status: NodeStatus,   // Track node state
    pub ts: u64,
    pub first_seen: u64,      // When first discovered (even as ephemeral)
    pub last_updated: u64,    // When last modified
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum NodeKind {
    SlackMessage,
    SlackThread,
    JiraTicket,
    JiraComment,
    ConfluencePage,
    GitHubPR,
    GitHubIssue,
    GitHubRepo,
    GitHubCommit,
    GoogleDoc,
    GoogleSheet,
    GoogleSlide,
    GenericUrl,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeMetadata {
    pub author: String,
    pub url: String,
    pub title: Option<String>,
    pub status: Option<String>, // For tickets/PRs
    pub mentioned_by: Vec<String>, // Track who mentioned this
    pub mention_count: u32,    // How many times mentioned
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RelationType {
    Mentions,    // Slack mentions Jira-123
    Implements,  // PR implements Confluence design
    Discusses,   // Thread about a document
    Fixes,       // Commit fixes Jira ticket
    References,  // Generic reference
    Contradicts, // AI-detected conflict
    Temporal,    // Time-based relationship
}

// Integration types
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum IntegrationType {
    Slack,
    Jira,
    Confluence,
    GitHub,
    GoogleDocs,
}

// Node cache for deduplication
pub struct NodeCache {
    nodes: Arc<RwLock<HashMap<String, Node>>>, // node_id -> Node
}

impl NodeCache {
    pub fn new() -> Self {
        Self {
            nodes: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    // Get or create ephemeral node
    pub async fn get_or_create_ephemeral(
        &self,
        external_id: &str,
        kind: NodeKind,
        workspace_id: &str,
        mentioned_by: &str,
    ) -> Node {
        let node_id = Self::generate_node_id(&kind, external_id);

        let mut nodes = self.nodes.write().await;

        if let Some(existing) = nodes.get_mut(&node_id) {
            // Node exists, just update mention tracking
            existing.metadata.mention_count += 1;
            if !existing.metadata.mentioned_by.contains(&mentioned_by.to_string()) {
                existing.metadata.mentioned_by.push(mentioned_by.to_string());
            }
            existing.last_updated = current_timestamp();
            return existing.clone();
        }

        // Create new ephemeral node
        let now = current_timestamp();
        let node = Node {
            id: node_id.clone(),
            workspace_id: workspace_id.to_string(),
            external_id: external_id.to_string(),
            name: format!("{:?} {}", kind, external_id),
            kind: kind.clone(),
            content: String::new(), // Empty for ephemeral
            metadata: NodeMetadata {
                author: String::new(),
                url: Self::generate_url(&kind, external_id),
                title: None,
                status: None,
                mentioned_by: vec![mentioned_by.to_string()],
                mention_count: 1,
            },
            status: NodeStatus::Ephemeral,
            ts: now,
            first_seen: now,
            last_updated: now,
        };

        nodes.insert(node_id, node.clone());
        node
    }

    // Update ephemeral node with real data
    pub async fn resolve_ephemeral(&self, external_id: &str, kind: NodeKind, real_data: Node) -> Node {
        let node_id = Self::generate_node_id(&kind, external_id);
        let mut nodes = self.nodes.write().await;

        if let Some(ephemeral) = nodes.get_mut(&node_id) {
            // Preserve ephemeral metadata while updating with real data
            let preserved_mentions = ephemeral.metadata.mentioned_by.clone();
            let preserved_count = ephemeral.metadata.mention_count;
            let first_seen = ephemeral.first_seen;

            // Update with real data
            *ephemeral = real_data;
            ephemeral.id = node_id.clone(); // Keep stable ID!
            ephemeral.status = NodeStatus::Resolved;
            ephemeral.first_seen = first_seen; // Preserve original discovery time
            ephemeral.metadata.mentioned_by = preserved_mentions;
            ephemeral.metadata.mention_count = preserved_count;
            ephemeral.last_updated = current_timestamp();

            return ephemeral.clone();
        }

        // No ephemeral node existed, just insert real one
        nodes.insert(node_id, real_data.clone());
        real_data
    }

    // Get all ephemeral nodes for batch resolution
    pub async fn get_ephemeral_nodes(&self, kind: NodeKind) -> Vec<Node> {
        let nodes = self.nodes.read().await;
        nodes.values()
            .filter(|n| n.kind == kind && n.status == NodeStatus::Ephemeral)
            .cloned()
            .collect()
    }

    // Generate stable node ID
    pub fn generate_node_id(kind: &NodeKind, external_id: &str) -> String {
        let prefix = match kind {
            NodeKind::JiraTicket | NodeKind::JiraComment => "jira",
            NodeKind::GitHubPR | NodeKind::GitHubIssue | NodeKind::GitHubRepo | NodeKind::GitHubCommit => "github",
            NodeKind::ConfluencePage => "confluence",
            NodeKind::GoogleDoc | NodeKind::GoogleSheet | NodeKind::GoogleSlide => "gdocs",
            NodeKind::SlackMessage | NodeKind::SlackThread => "slack",
            NodeKind::GenericUrl => "url",
        };
        blake3::hash(format!("{}:{}", prefix, external_id).as_bytes())
            .to_hex()
            .to_string()
    }

    // Generate URL for ephemeral nodes
    fn generate_url(kind: &NodeKind, external_id: &str) -> String {
        match kind {
            NodeKind::JiraTicket => format!("https://jira.example.com/browse/{}", external_id),
            NodeKind::GitHubPR => format!("https://github.com/org/repo/pull/{}", external_id),
            NodeKind::GitHubIssue => format!("https://github.com/org/repo/issues/{}", external_id),
            NodeKind::GitHubCommit => format!("https://github.com/org/repo/commit/{}", external_id),
            NodeKind::ConfluencePage => format!("https://confluence.example.com/pages/{}", external_id),
            NodeKind::GoogleDoc => format!("https://docs.google.com/document/d/{}", external_id),
            NodeKind::GoogleSheet => format!("https://docs.google.com/spreadsheets/d/{}", external_id),
            NodeKind::GoogleSlide => format!("https://docs.google.com/presentation/d/{}", external_id),
            _ => String::new(),
        }
    }
}

// Generic handle for integration with specific command type
struct IntegrationHandle<C> {
    command_tx: UnboundedSender<C>,
    event_rx: Arc<RwLock<UnboundedReceiver<IntegrationEvent>>>,
    node_rx: Arc<RwLock<UnboundedReceiver<Node>>>,
}

// Main integration manager
pub struct IntegrationManager {
    slack_actors: Arc<RwLock<HashMap<String, IntegrationHandle<SlackCommand>>>>,
    jira_actors: Arc<RwLock<HashMap<String, IntegrationHandle<JiraCommand>>>>,
    github_actors: Arc<RwLock<HashMap<String, IntegrationHandle<GitHubCommand>>>>,
    confluence_actors: Arc<RwLock<HashMap<String, IntegrationHandle<ConfluenceCommand>>>>,
    googledocs_actors: Arc<RwLock<HashMap<String, IntegrationHandle<GoogleDocsCommand>>>>,
    node_cache: Arc<NodeCache>,
}

impl IntegrationManager {
    pub fn new() -> Self {
        Self {
            slack_actors: Arc::new(RwLock::new(HashMap::new())),
            jira_actors: Arc::new(RwLock::new(HashMap::new())),
            github_actors: Arc::new(RwLock::new(HashMap::new())),
            confluence_actors: Arc::new(RwLock::new(HashMap::new())),
            googledocs_actors: Arc::new(RwLock::new(HashMap::new())),
            node_cache: Arc::new(NodeCache::new()),
        }
    }

    // Start Slack integration for a workspace
    pub async fn start_slack(
        &self,
        workspace_id: String,
        token: String,
        channels: Vec<String>,
    ) -> Result<(), String> {
        // Check if already exists
        if self.slack_actors.read().await.contains_key(&workspace_id) {
            return Err("Slack already running for this workspace".to_string());
        }

        // Create channels with specific command type
        let (cmd_tx, cmd_rx) = unbounded_channel::<SlackCommand>();
        let (event_tx, event_rx) = unbounded_channel();
        let (node_tx, node_rx) = unbounded_channel();

        // Create and spawn actor
        let actor = SlackActor::new(
            token,
            workspace_id.clone(),
            Duration::from_secs(5),
            cmd_rx,
            event_tx,
            node_tx,
        );

        // Add initial channels
        for channel in channels {
            cmd_tx.send(SlackCommand::AddChannel(channel))
                .map_err(|e| e.to_string())?;
        }

        // Spawn the actor
        tokio::spawn(async move {
            actor.run().await;
        });

        // Store handle
        let handle = IntegrationHandle {
            command_tx: cmd_tx,
            event_rx: Arc::new(RwLock::new(event_rx)),
            node_rx: Arc::new(RwLock::new(node_rx)),
        };

        self.slack_actors.write().await.insert(workspace_id, handle);
        Ok(())
    }

    // Start JIRA integration for a workspace
    pub async fn start_jira(
        &self,
        workspace_id: String,
        domain: String,
        email: String,
        api_token: String,
        projects: Vec<String>,
    ) -> Result<(), String> {
        // Check if already exists
        if self.jira_actors.read().await.contains_key(&workspace_id) {
            return Err("JIRA already running for this workspace".to_string());
        }

        // Create channels with specific command type
        let (cmd_tx, cmd_rx) = unbounded_channel::<JiraCommand>();
        let (event_tx, event_rx) = unbounded_channel();
        let (node_tx, node_rx) = unbounded_channel();

        // Pass node cache to actor
        let node_cache = self.node_cache.clone();

        // Create and spawn actor
        let actor = JiraActor::new(
            domain,
            email,
            api_token,
            workspace_id.clone(),
            Duration::from_secs(60),
            cmd_rx,
            event_tx,
            node_tx,
            node_cache,
        );

        // Add initial projects
        for project in projects {
            cmd_tx.send(JiraCommand::AddProject(project))
                .map_err(|e| e.to_string())?;
        }

        // Spawn the actor
        tokio::spawn(async move {
            actor.run().await;
        });

        // Store handle
        let handle = IntegrationHandle {
            command_tx: cmd_tx,
            event_rx: Arc::new(RwLock::new(event_rx)),
            node_rx: Arc::new(RwLock::new(node_rx)),
        };

        self.jira_actors.write().await.insert(workspace_id, handle);
        Ok(())
    }

    // Start GitHub integration for a workspace
    pub async fn start_github(
        &self,
        workspace_id: String,
        token: String,
        repos: Vec<(String, String)>, // (owner, repo) pairs
    ) -> Result<(), String> {
        // Check if already exists
        if self.github_actors.read().await.contains_key(&workspace_id) {
            return Err("GitHub already running for this workspace".to_string());
        }

        // Create channels with specific command type
        let (cmd_tx, cmd_rx) = unbounded_channel::<GitHubCommand>();
        let (event_tx, event_rx) = unbounded_channel();
        let (node_tx, node_rx) = unbounded_channel();

        // Pass node cache to actor
        let node_cache = self.node_cache.clone();

        // Create and spawn actor
        let actor = GitHubActor::new(
            token,
            workspace_id.clone(),
            Duration::from_secs(60),
            cmd_rx,
            event_tx,
            node_tx,
            node_cache,
        );

        // Add initial repos
        for (owner, repo) in repos {
            cmd_tx.send(GitHubCommand::AddRepo(owner, repo))
                .map_err(|e| e.to_string())?;
        }

        // Spawn the actor
        tokio::spawn(async move {
            actor.run().await;
        });

        // Store handle
        let handle = IntegrationHandle {
            command_tx: cmd_tx,
            event_rx: Arc::new(RwLock::new(event_rx)),
            node_rx: Arc::new(RwLock::new(node_rx)),
        };

        self.github_actors.write().await.insert(workspace_id, handle);
        Ok(())
    }

    // Start Confluence integration for a workspace
    pub async fn start_confluence(
        &self,
        workspace_id: String,
        domain: String,
        email: String,
        api_token: String,
        spaces: Vec<String>,
    ) -> Result<(), String> {
        // Check if already exists
        if self.confluence_actors.read().await.contains_key(&workspace_id) {
            return Err("Confluence already running for this workspace".to_string());
        }

        // Create channels with specific command type
        let (cmd_tx, cmd_rx) = unbounded_channel::<ConfluenceCommand>();
        let (event_tx, event_rx) = unbounded_channel();
        let (node_tx, node_rx) = unbounded_channel();

        // Pass node cache to actor
        let node_cache = self.node_cache.clone();

        // Create and spawn actor
        let actor = ConfluenceActor::new(
            domain,
            email,
            api_token,
            workspace_id.clone(),
            Duration::from_secs(120), // Poll every 2 minutes
            cmd_rx,
            event_tx,
            node_tx,
            node_cache,
        );

        // Add initial spaces
        for space in spaces {
            cmd_tx.send(ConfluenceCommand::AddSpace(space))
                .map_err(|e| e.to_string())?;
        }

        // Spawn the actor
        tokio::spawn(async move {
            actor.run().await;
        });

        // Store handle
        let handle = IntegrationHandle {
            command_tx: cmd_tx,
            event_rx: Arc::new(RwLock::new(event_rx)),
            node_rx: Arc::new(RwLock::new(node_rx)),
        };

        self.confluence_actors.write().await.insert(workspace_id, handle);
        Ok(())
    }

    // Start Google Docs integration for a workspace
    pub async fn start_googledocs(
        &self,
        workspace_id: String,
        access_token: String,
        document_ids: Vec<String>,
    ) -> Result<(), String> {
        // Check if already exists
        if self.googledocs_actors.read().await.contains_key(&workspace_id) {
            return Err("Google Docs already running for this workspace".to_string());
        }

        // Create channels with specific command type
        let (cmd_tx, cmd_rx) = unbounded_channel::<GoogleDocsCommand>();
        let (event_tx, event_rx) = unbounded_channel();
        let (node_tx, node_rx) = unbounded_channel();

        // Pass node cache to actor
        let node_cache = self.node_cache.clone();

        // Create and spawn actor
        let actor = GoogleDocsActor::new(
            access_token,
            workspace_id.clone(),
            Duration::from_secs(120), // Poll every 2 minutes
            cmd_rx,
            event_tx,
            node_tx,
            node_cache,
        );

        // Add initial documents
        for doc_id in document_ids {
            cmd_tx.send(GoogleDocsCommand::AddDocument(doc_id))
                .map_err(|e| e.to_string())?;
        }

        // Spawn the actor
        tokio::spawn(async move {
            actor.run().await;
        });

        // Store handle
        let handle = IntegrationHandle {
            command_tx: cmd_tx,
            event_rx: Arc::new(RwLock::new(event_rx)),
            node_rx: Arc::new(RwLock::new(node_rx)),
        };

        self.googledocs_actors.write().await.insert(workspace_id, handle);
        Ok(())
    }

    // Generic get events method
    async fn get_events_from<C>(
        &self,
        actors: &Arc<RwLock<HashMap<String, IntegrationHandle<C>>>>,
        workspace_id: &str,
    ) -> Vec<IntegrationEvent> {
        let actors = actors.read().await;
        if let Some(handle) = actors.get(workspace_id) {
            let mut events = Vec::new();
            let mut rx = handle.event_rx.write().await;

            while let Ok(event) = rx.try_recv() {
                events.push(event);
                if events.len() >= 100 { break; }
            }
            events
        } else {
            Vec::new()
        }
    }

    // Generic get nodes method
    async fn get_nodes_from<C>(
        &self,
        actors: &Arc<RwLock<HashMap<String, IntegrationHandle<C>>>>,
        workspace_id: &str,
    ) -> Vec<Node> {
        let actors = actors.read().await;
        if let Some(handle) = actors.get(workspace_id) {
            let mut nodes = Vec::new();
            let mut rx = handle.node_rx.write().await;

            while let Ok(node) = rx.try_recv() {
                nodes.push(node);
                if nodes.len() >= 100 { break; }
            }
            nodes
        } else {
            Vec::new()
        }
    }

    // Get events from all integrations
    pub async fn get_all_events(&self, workspace_id: &str) -> Vec<IntegrationEvent> {
        let mut all_events = Vec::new();

        all_events.extend(self.get_events_from(&self.slack_actors, workspace_id).await);
        all_events.extend(self.get_events_from(&self.jira_actors, workspace_id).await);
        all_events.extend(self.get_events_from(&self.github_actors, workspace_id).await);
        all_events.extend(self.get_events_from(&self.confluence_actors, workspace_id).await);
        all_events.extend(self.get_events_from(&self.googledocs_actors, workspace_id).await);

        all_events
    }

    // Get nodes from all integrations
    pub async fn get_all_nodes(&self, workspace_id: &str) -> Vec<Node> {
        let mut all_nodes = Vec::new();

        all_nodes.extend(self.get_nodes_from(&self.slack_actors, workspace_id).await);
        all_nodes.extend(self.get_nodes_from(&self.jira_actors, workspace_id).await);
        all_nodes.extend(self.get_nodes_from(&self.github_actors, workspace_id).await);
        all_nodes.extend(self.get_nodes_from(&self.confluence_actors, workspace_id).await);
        all_nodes.extend(self.get_nodes_from(&self.googledocs_actors, workspace_id).await);

        all_nodes
    }

    // Get Slack-specific events
    pub async fn get_slack_events(&self, workspace_id: &str) -> Vec<IntegrationEvent> {
        self.get_events_from(&self.slack_actors, workspace_id).await
    }

    // Get Slack-specific nodes
    pub async fn get_slack_nodes(&self, workspace_id: &str) -> Vec<Node> {
        self.get_nodes_from(&self.slack_actors, workspace_id).await
    }

    // Stop Slack for a workspace
    pub async fn stop_slack(&self, workspace_id: &str) -> Result<(), String> {
        let mut actors = self.slack_actors.write().await;
        if let Some(handle) = actors.remove(workspace_id) {
            handle.command_tx.send(SlackCommand::Shutdown)
                .map_err(|e| e.to_string())
        } else {
            Err("Slack not running for this workspace".to_string())
        }
    }

    // Stop JIRA for a workspace
    pub async fn stop_jira(&self, workspace_id: &str) -> Result<(), String> {
        let mut actors = self.jira_actors.write().await;
        if let Some(handle) = actors.remove(workspace_id) {
            handle.command_tx.send(JiraCommand::Shutdown)
                .map_err(|e| e.to_string())
        } else {
            Err("JIRA not running for this workspace".to_string())
        }
    }

    // Stop GitHub for a workspace
    pub async fn stop_github(&self, workspace_id: &str) -> Result<(), String> {
        let mut actors = self.github_actors.write().await;
        if let Some(handle) = actors.remove(workspace_id) {
            handle.command_tx.send(GitHubCommand::Shutdown)
                .map_err(|e| e.to_string())
        } else {
            Err("GitHub not running for this workspace".to_string())
        }
    }

    // Stop Confluence for a workspace
    pub async fn stop_confluence(&self, workspace_id: &str) -> Result<(), String> {
        let mut actors = self.confluence_actors.write().await;
        if let Some(handle) = actors.remove(workspace_id) {
            handle.command_tx.send(ConfluenceCommand::Shutdown)
                .map_err(|e| e.to_string())
        } else {
            Err("Confluence not running for this workspace".to_string())
        }
    }

    // Stop Google Docs for a workspace
    pub async fn stop_googledocs(&self, workspace_id: &str) -> Result<(), String> {
        let mut actors = self.googledocs_actors.write().await;
        if let Some(handle) = actors.remove(workspace_id) {
            handle.command_tx.send(GoogleDocsCommand::Shutdown)
                .map_err(|e| e.to_string())
        } else {
            Err("Google Docs not running for this workspace".to_string())
        }
    }

    // Stop all integrations for a workspace
    pub async fn stop_all(&self, workspace_id: &str) {
        let _ = self.stop_slack(workspace_id).await;
        let _ = self.stop_jira(workspace_id).await;
        let _ = self.stop_github(workspace_id).await;
        let _ = self.stop_confluence(workspace_id).await;
        let _ = self.stop_googledocs(workspace_id).await;
    }

    // Add channel to existing Slack integration
    pub async fn add_slack_channel(&self, workspace_id: &str, channel: String) -> Result<(), String> {
        let actors = self.slack_actors.read().await;
        if let Some(handle) = actors.get(workspace_id) {
            handle.command_tx.send(SlackCommand::AddChannel(channel))
                .map_err(|e| e.to_string())
        } else {
            Err("Slack not running for this workspace".to_string())
        }
    }

    // Get all ephemeral nodes that need resolution
    pub async fn get_ephemeral_nodes(&self) -> HashMap<NodeKind, Vec<Node>> {
        let mut result = HashMap::new();

        result.insert(NodeKind::JiraTicket,
            self.node_cache.get_ephemeral_nodes(NodeKind::JiraTicket).await);
        result.insert(NodeKind::GitHubPR,
            self.node_cache.get_ephemeral_nodes(NodeKind::GitHubPR).await);
        result.insert(NodeKind::GitHubIssue,
            self.node_cache.get_ephemeral_nodes(NodeKind::GitHubIssue).await);
        result.insert(NodeKind::ConfluencePage,
            self.node_cache.get_ephemeral_nodes(NodeKind::ConfluencePage).await);
        result.insert(NodeKind::GoogleDoc,
            self.node_cache.get_ephemeral_nodes(NodeKind::GoogleDoc).await);
        result.insert(NodeKind::GoogleSheet,
            self.node_cache.get_ephemeral_nodes(NodeKind::GoogleSheet).await);
        result.insert(NodeKind::GoogleSlide,
            self.node_cache.get_ephemeral_nodes(NodeKind::GoogleSlide).await);

        result
    }

    // Resolve ephemeral nodes when integration is added
    pub async fn resolve_ephemeral_nodes(&self, kind: NodeKind, resolved_nodes: Vec<Node>) {
        for node in resolved_nodes {
            let external_id = node.external_id.clone();
            self.node_cache.resolve_ephemeral(&external_id, kind.clone(), node).await;
        }
    }

    // Poll and sync for Cyan backend compatibility
    pub async fn poll_and_sync(&self, workspace_id: &str) -> (Vec<Event>, Vec<Node>) {
        let events = self.get_all_events(workspace_id).await;
        let nodes = self.get_all_nodes(workspace_id).await;

        // Convert to XaeroFlux events
        let xf_events: Vec<Event> = events.iter()
            .map(|e| e.to_xaeroflux_event())
            .collect();

        (xf_events, nodes)
    }
}

// For your Cyan backend to convert to XaeroFlux events
impl IntegrationEvent {
    pub fn to_xaeroflux_event(&self) -> Event {
        self.base.clone()
    }
}

// Helper function for timestamps
pub fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// Helper function for extracting context around a position in text
pub fn extract_context(text: &str, position: usize, context_size: usize) -> String {
    // Find valid char boundary for start
    let mut start = position.saturating_sub(context_size);
    while start > 0 && !text.is_char_boundary(start) {
        start -= 1;
    }

    // Find valid char boundary for end
    let mut end = (position + context_size).min(text.len());
    while end < text.len() && !text.is_char_boundary(end) {
        end += 1;
    }

    text[start..end].to_string()
}

// Helper function for extracting domain from URL
pub fn extract_domain(url: &str) -> String {
    url.split('/')
        .nth(2)
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_string()
}