pub mod slack;
pub mod jira;
pub mod github;
pub(crate) mod confluence;
pub(crate) mod googledocs;

pub use slack::SlackClient;
pub use jira::JiraClient;
pub use github::GitHubClient;