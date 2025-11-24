use reqwest;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubRepo {
    pub id: u64,
    pub name: String,
    pub full_name: String,
    pub owner: GitHubUser,
    pub description: Option<String>,
    pub html_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubUser {
    pub login: String,
    pub id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubPullRequest {
    pub id: u64,
    pub number: u32,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub user: GitHubUser,
    pub created_at: String,
    pub updated_at: String,
    pub html_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubIssue {
    pub id: u64,
    pub number: u32,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub user: GitHubUser,
    pub assignee: Option<GitHubUser>,
    pub created_at: String,
    pub updated_at: String,
    pub html_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubCommit {
    pub sha: String,
    pub commit: CommitDetails,
    pub author: Option<GitHubUser>,
    pub html_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitDetails {
    pub message: String,
    pub author: CommitAuthor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitAuthor {
    pub name: String,
    pub email: String,
    pub date: String,
}

pub struct GitHubClient {
    client: reqwest::Client,
    token: String,
    base_url: String,
}

impl GitHubClient {
    pub fn new(token: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            token,
            base_url: "https://api.github.com".to_string(),
        }
    }

    pub async fn get_repo(&self, owner: &str, repo: &str) -> Result<GitHubRepo, Box<dyn std::error::Error + Send + Sync>> {
        let url = format!("{}/repos/{}/{}", self.base_url, owner, repo);

        let response = self.client
            .get(&url)
            .header("Authorization", format!("token {}", self.token))
            .header("User-Agent", "cyan-integrations")
            .send()
            .await?
            .json::<GitHubRepo>()
            .await?;

        Ok(response)
    }

    pub async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u32
    ) -> Result<GitHubPullRequest, Box<dyn std::error::Error + Send + Sync>> {
        let url = format!("{}/repos/{}/{}/pulls/{}", self.base_url, owner, repo, pr_number);

        let response = self.client
            .get(&url)
            .header("Authorization", format!("token {}", self.token))
            .header("User-Agent", "cyan-integrations")
            .send()
            .await?
            .json::<GitHubPullRequest>()
            .await?;

        Ok(response)
    }

    pub async fn get_issue(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u32
    ) -> Result<GitHubIssue, Box<dyn std::error::Error + Send + Sync>> {
        let url = format!("{}/repos/{}/{}/issues/{}", self.base_url, owner, repo, issue_number);

        let response = self.client
            .get(&url)
            .header("Authorization", format!("token {}", self.token))
            .header("User-Agent", "cyan-integrations")
            .send()
            .await?
            .json::<GitHubIssue>()
            .await?;

        Ok(response)
    }

    pub async fn list_commits(
        &self,
        owner: &str,
        repo: &str,
        since: Option<&str>
    ) -> Result<Vec<GitHubCommit>, Box<dyn std::error::Error + Send + Sync>> {
        let mut url = format!("{}/repos/{}/{}/commits", self.base_url, owner, repo);

        if let Some(since_date) = since {
            url.push_str(&format!("?since={}", since_date));
        }

        let response = self.client
            .get(&url)
            .header("Authorization", format!("token {}", self.token))
            .header("User-Agent", "cyan-integrations")
            .send()
            .await?
            .json::<Vec<GitHubCommit>>()
            .await?;

        Ok(response)
    }
}