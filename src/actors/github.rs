use crate::clients::github::{GitHubClient, GitHubPullRequest};
use crate::{Node, NodeKind, NodeStatus, NodeMetadata, Event, IntegrationEvent, RelationType, NodeCache, current_timestamp};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

pub struct GitHubActor {
    workspace_id: String,
    repos: HashSet<(String, String)>, // (owner, repo) pairs
    poll_interval: Duration,
    client: Arc<GitHubClient>,
    commands: UnboundedReceiver<GitHubCommand>,
    events: UnboundedSender<IntegrationEvent>,
    nodes: UnboundedSender<Node>,
    node_cache: Arc<NodeCache>,
    last_sync: HashMap<String, String>,
}

pub enum GitHubCommand {
    AddRepo(String, String), // owner, repo
    RemoveRepo(String, String),
    ResolveEphemeralNodes,
    Shutdown,
}

impl GitHubActor {
    pub fn new(
        token: String,
        workspace_id: String,
        poll_interval: Duration,
        commands: UnboundedReceiver<GitHubCommand>,
        events: UnboundedSender<IntegrationEvent>,
        nodes: UnboundedSender<Node>,
        node_cache: Arc<NodeCache>,
    ) -> Self {
        Self {
            workspace_id,
            repos: HashSet::new(),
            poll_interval,
            client: Arc::new(GitHubClient::new(token)),
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
                    if let Err(e) = self.sync_repos().await {
                        eprintln!("GitHub sync error: {:?}", e);
                    }

                    if let Err(e) = self.resolve_ephemeral_nodes().await {
                        eprintln!("Failed to resolve ephemeral nodes: {:?}", e);
                    }
                }
                Some(cmd) = self.commands.recv() => {
                    match cmd {
                        GitHubCommand::AddRepo(owner, repo) => {
                            self.repos.insert((owner, repo));
                        }
                        GitHubCommand::RemoveRepo(owner, repo) => {
                            self.repos.remove(&(owner, repo));
                        }
                        GitHubCommand::ResolveEphemeralNodes => {
                            let _ = self.resolve_ephemeral_nodes().await;
                        }
                        GitHubCommand::Shutdown => break,
                    }
                }
            }
        }
    }

    async fn sync_repos(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for (owner, repo) in &self.repos {
            // Get recent commits
            let commits = self.client.list_commits(owner, repo, None).await?;

            for commit in commits.iter().take(20) {
                // Look for JIRA references in commit messages
                let events = self.extract_events_from_commit(commit);
                for event in events {
                    self.events.send(event)?;
                }
            }
        }

        Ok(())
    }

    async fn resolve_ephemeral_nodes(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Resolve ephemeral PR nodes
        let ephemeral_prs = self.node_cache.get_ephemeral_nodes(NodeKind::GitHubPR).await;

        for ghost_node in ephemeral_prs {
            // Parse PR number from external_id
            if let Ok(pr_num) = ghost_node.external_id.parse::<u32>() {
                // Try each repo (we don't know which one)
                for (owner, repo) in &self.repos {
                    match self.client.get_pull_request(owner, repo, pr_num).await {
                        Ok(pr) => {
                            let real_node = self.create_node_from_pr(&pr, owner, repo);
                            let resolved = self.node_cache.resolve_ephemeral(
                                &ghost_node.external_id,
                                NodeKind::GitHubPR,
                                real_node
                            ).await;

                            self.nodes.send(resolved)?;
                            break; // Found it, stop searching
                        }
                        Err(_) => continue, // Try next repo
                    }
                }
            }
        }

        Ok(())
    }

    fn create_node_from_pr(&self, pr: &GitHubPullRequest, owner: &str, repo: &str) -> Node {
        let external_id = pr.number.to_string();
        let node_id = NodeCache::generate_node_id(&NodeKind::GitHubPR, &external_id);

        Node {
            id: node_id,
            workspace_id: self.workspace_id.clone(),
            external_id,
            name: format!("PR #{}: {}", pr.number, pr.title),
            kind: NodeKind::GitHubPR,
            content: serde_json::json!({
                "title": pr.title,
                "body": pr.body,
                "state": pr.state,
                "author": pr.user.login,
                "repo": format!("{}/{}", owner, repo),
            }).to_string(),
            metadata: NodeMetadata {
                author: pr.user.login.clone(),
                url: pr.html_url.clone(),
                title: Some(pr.title.clone()),
                status: Some(pr.state.clone()),
                mentioned_by: vec![],
                mention_count: 0,
            },
            status: NodeStatus::Real,
            ts: chrono::DateTime::parse_from_rfc3339(&pr.created_at)
                .map(|dt| dt.timestamp() as u64)
                .unwrap_or(0),
            first_seen: current_timestamp(),
            last_updated: current_timestamp(),
        }
    }

    fn extract_events_from_commit(&self, commit: &crate::clients::github::GitHubCommit) -> Vec<IntegrationEvent> {
        let mut events = Vec::new();

        // Look for JIRA references in commit message
        let jira_regex = regex::Regex::new(r"\b([A-Z]{2,}-\d+)\b").unwrap();
        for capture in jira_regex.captures_iter(&commit.commit.message) {
            if let Some(jira_id) = capture.get(1) {
                let event_id = blake3::hash(
                    format!("commit:{}:jira:{}", commit.sha, jira_id.as_str()).as_bytes()
                ).to_hex().to_string();

                events.push(IntegrationEvent {
                    base: Event {
                        id: event_id,
                        payload: serde_json::json!({
                            "type": "commit_fixes_issue",
                            "commit_sha": commit.sha,
                            "jira_key": jira_id.as_str(),
                            "message": commit.commit.message,
                        }).to_string(),
                        source: commit.sha.clone(),
                        ts: current_timestamp(),
                    },
                    workspace_id: self.workspace_id.clone(),
                    relation: RelationType::Fixes,
                    confidence: 0.9,
                    discovered_at: current_timestamp(),
                });
            }
        }

        events
    }
}