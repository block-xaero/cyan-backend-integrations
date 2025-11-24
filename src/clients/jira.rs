use reqwest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraIssue {
    pub key: String,
    pub id: String,
    pub fields: JiraFields,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraFields {
    pub summary: String,
    pub description: Option<String>,
    pub status: JiraStatus,
    pub assignee: Option<JiraUser>,
    pub reporter: Option<JiraUser>,
    pub created: String,
    pub updated: String,
    pub priority: Option<JiraPriority>,
    pub comment: Option<JiraComments>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraStatus {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraUser {
    pub account_id: String,
    pub display_name: String,
    pub email_address: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraPriority {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraComments {
    pub comments: Vec<JiraComment>,
    pub total: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraComment {
    pub id: String,
    pub author: JiraUser,
    pub body: String,
    pub created: String,
}

pub struct JiraClient {
    client: reqwest::Client,
    base_url: String,
    email: String,
    api_token: String,
}

impl JiraClient {
    pub fn new(domain: String, email: String, api_token: String) -> Self {
        let base_url = format!("https://{}.atlassian.net/rest/api/3", domain);
        Self {
            client: reqwest::Client::new(),
            base_url,
            email,
            api_token,
        }
    }

    pub async fn get_issue(&self, issue_key: &str) -> Result<JiraIssue, Box<dyn std::error::Error + Send + Sync>> {
        let url = format!("{}/issue/{}", self.base_url, issue_key);

        let response = self.client
            .get(&url)
            .basic_auth(&self.email, Some(&self.api_token))
            .send()
            .await?
            .json::<JiraIssue>()
            .await?;

        Ok(response)
    }

    pub async fn search_issues(&self, jql: &str) -> Result<Vec<JiraIssue>, Box<dyn std::error::Error + Send + Sync>> {
        let url = format!("{}/search", self.base_url);

        let mut params = HashMap::new();
        params.insert("jql", jql);
        params.insert("maxResults", "50");

        let response = self.client
            .get(&url)
            .basic_auth(&self.email, Some(&self.api_token))
            .query(&params)
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;

        let issues = response["issues"]
            .as_array()
            .ok_or("No issues found")?
            .iter()
            .filter_map(|v| serde_json::from_value::<JiraIssue>(v.clone()).ok())
            .collect();

        Ok(issues)
    }

    pub async fn get_projects(&self) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error + Send + Sync>> {
        let url = format!("{}/project", self.base_url);

        let response = self.client
            .get(&url)
            .basic_auth(&self.email, Some(&self.api_token))
            .send()
            .await?
            .json::<Vec<serde_json::Value>>()
            .await?;

        Ok(response)
    }
}