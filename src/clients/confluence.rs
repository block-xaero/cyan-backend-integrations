use reqwest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfluencePage {
    pub id: String,
    pub title: String,
    pub space: SpaceInfo,
    pub version: VersionInfo,
    pub body: Option<BodyContent>,
    #[serde(rename = "_links")]
    pub links: PageLinks,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceInfo {
    pub key: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    pub number: u32,
    pub when: String,
    pub by: Option<ConfluenceUser>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfluenceUser {
    pub username: Option<String>,
    pub display_name: String,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BodyContent {
    pub storage: StorageContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageContent {
    pub value: String,
    pub representation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageLinks {
    pub webui: String,
    #[serde(rename = "self")]
    pub self_link: String,
}

pub struct ConfluenceClient {
    client: reqwest::Client,
    base_url: String,
    email: String,
    api_token: String,
}

impl ConfluenceClient {
    pub fn new(domain: String, email: String, api_token: String) -> Self {
        let base_url = format!("https://{}.atlassian.net/wiki/rest/api", domain);
        Self {
            client: reqwest::Client::new(),
            base_url,
            email,
            api_token,
        }
    }

    pub async fn get_page(&self, page_id: &str) -> Result<ConfluencePage, Box<dyn std::error::Error + Send + Sync>> {
        let url = format!("{}/content/{}?expand=body.storage,version,space", self.base_url, page_id);

        let response = self.client
            .get(&url)
            .basic_auth(&self.email, Some(&self.api_token))
            .send()
            .await?
            .json::<ConfluencePage>()
            .await?;

        Ok(response)
    }

    pub async fn search_content(&self, cql: &str) -> Result<Vec<ConfluencePage>, Box<dyn std::error::Error + Send + Sync>> {
        let url = format!("{}/content/search", self.base_url);

        let mut params = HashMap::new();
        params.insert("cql", cql);
        params.insert("limit", "50");

        let response = self.client
            .get(&url)
            .basic_auth(&self.email, Some(&self.api_token))
            .query(&params)
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;

        let pages = response["results"]
            .as_array()
            .ok_or("No results found")?
            .iter()
            .filter_map(|v| serde_json::from_value::<ConfluencePage>(v.clone()).ok())
            .collect();

        Ok(pages)
    }

    pub async fn get_space_pages(&self, space_key: &str) -> Result<Vec<ConfluencePage>, Box<dyn std::error::Error + Send + Sync>> {
        let cql = format!("space = {} and type = page", space_key);
        self.search_content(&cql).await
    }
}