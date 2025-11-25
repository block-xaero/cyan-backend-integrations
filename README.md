# cyan-backend-integrations

Rust library for integrating external collaboration tools (Slack, JIRA, GitHub, Confluence, Google Docs) into a unified event stream.

## Features

- Polls external services for updates
- Converts data into unified Node/Event model
- Handles ephemeral nodes (mentioned but not yet fetched items)
- Manages cross-service references (JIRA tickets mentioned in Slack, etc.)

## Installation

```toml
[dependencies]
cyan-backend-integrations = { path = "../cyan-backend-integrations" }
```

## Usage

```rust
use cyan_backend_integrations::{IntegrationManager, Node, Event};

#[tokio::main]
async fn main() {
    let manager = IntegrationManager::new();
    
    // Start Slack integration
    manager.start_slack(
        "workspace-id".to_string(),
        "xoxb-slack-token".to_string(),
        vec!["C1234567890".to_string()], // Channel IDs
    ).await.unwrap();
    
    // Poll for data
    let (events, nodes) = manager.poll_and_sync("workspace-id").await;
}
```

## Supported Integrations

### Slack
- Requires bot token with `channels:history` and `channels:read` scopes
- Monitors specified channels for messages
- Extracts mentions of JIRA tickets, GitHub PRs, etc.

### JIRA
- Requires email and API token
- Monitors specified projects for issues
- Extracts GitHub PR references from descriptions

### GitHub
- Requires personal access token
- Monitors repositories for commits and PRs
- Extracts JIRA ticket references from commit messages

### Confluence
- Requires email and API token
- Monitors spaces for page updates
- Extracts JIRA and GitHub references

### Google Docs
- Requires OAuth access token
- Monitors specified documents
- Extracts cross-references

## Data Model

### Node
Represents a piece of content (message, ticket, document):
- `id`: Stable Blake3 hash
- `kind`: Type of content (SlackMessage, JiraTicket, etc.)
- `status`: Real, Ephemeral, Resolved, NotFound
- `metadata`: Author, URL, title, status

### Event
Represents relationships between nodes:
- `source`: Node ID that created the event
- `relation`: Type of relationship (Mentions, Implements, Fixes)
- `confidence`: Confidence score (0.0-1.0)

## Architecture

Each integration runs as an independent actor with:
- Periodic polling (configurable interval)
- Command channel for runtime control
- Event/node output channels
- Shared node cache for deduplication

## License
Copyright (c) Blockxaero Inc. All rights reserved.
