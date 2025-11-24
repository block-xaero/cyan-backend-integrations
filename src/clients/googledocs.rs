use reqwest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleDoc {
    pub document_id: String,
    pub title: String,
    pub body: Option<DocumentBody>,
    pub revision_id: Option<String>,
    pub last_modified_time: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentBody {
    pub content: Vec<StructuralElement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuralElement {
    pub paragraph: Option<Paragraph>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Paragraph {
    pub elements: Vec<ParagraphElement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParagraphElement {
    pub text_run: Option<TextRun>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextRun {
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleSheet {
    pub spreadsheet_id: String,
    pub properties: SpreadsheetProperties,
    pub sheets: Vec<Sheet>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpreadsheetProperties {
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sheet {
    pub properties: SheetProperties,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SheetProperties {
    pub title: String,
    pub sheet_id: u32,
}

pub struct GoogleDocsClient {
    client: reqwest::Client,
    access_token: String,
    docs_base_url: String,
    sheets_base_url: String,
    drive_base_url: String,
}

impl GoogleDocsClient {
    pub fn new(access_token: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            access_token,
            docs_base_url: "https://docs.googleapis.com/v1".to_string(),
            sheets_base_url: "https://sheets.googleapis.com/v4".to_string(),
            drive_base_url: "https://www.googleapis.com/drive/v3".to_string(),
        }
    }

    pub async fn get_document(&self, document_id: &str) -> Result<GoogleDoc, Box<dyn std::error::Error + Send + Sync>> {
        let url = format!("{}/documents/{}", self.docs_base_url, document_id);

        let response = self.client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .send()
            .await?
            .json::<GoogleDoc>()
            .await?;

        Ok(response)
    }

    pub async fn get_spreadsheet(&self, spreadsheet_id: &str) -> Result<GoogleSheet, Box<dyn std::error::Error + Send + Sync>> {
        let url = format!("{}/spreadsheets/{}", self.sheets_base_url, spreadsheet_id);

        let response = self.client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .send()
            .await?
            .json::<GoogleSheet>()
            .await?;

        Ok(response)
    }

    pub async fn list_files(&self, query: Option<&str>) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error + Send + Sync>> {
        let url = format!("{}/files", self.drive_base_url);

        let mut params = HashMap::new();
        if let Some(q) = query {
            params.insert("q", q);
        }
        params.insert("fields", "files(id,name,mimeType,modifiedTime)");

        let response = self.client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .query(&params)
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;

        let files = response["files"]
            .as_array()
            .ok_or("No files found")?
            .to_vec();

        Ok(files)
    }
}