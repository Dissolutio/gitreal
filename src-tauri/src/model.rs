use serde::Serialize;
use std::sync::Arc;

#[derive(Clone, Debug, Serialize)]
pub struct JobAction {
    pub kind: String,
    pub label: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, thiserror::Error)]
#[error("{message}")]
pub struct BackendError {
    pub code: &'static str,
    pub message: String,
}

impl BackendError {
    pub fn new(code: &'static str, message: impl ToString) -> Self {
        Self {
            code,
            message: message.to_string(),
        }
    }

    pub fn git(error: impl std::fmt::Display) -> Self {
        Self::new("git_error", error)
    }
}

pub type Result<T> = std::result::Result<T, BackendError>;
pub type JobId = String;

#[derive(Clone, Debug, Serialize)]
pub struct OpenRepo {
    pub repo_id: String,
    pub path: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RefInfo {
    pub name: String,
    pub full_name: String,
    pub kind: &'static str,
    pub target: Option<String>,
    pub is_head: bool,
    pub upstream: Option<String>,
    pub ahead: Option<usize>,
    pub behind: Option<usize>,
    pub upstream_gone: bool,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct GraphEdge {
    pub from_lane: usize,
    pub to_lane: usize,
    pub parent_id: Arc<str>,
}

#[derive(Clone, Debug, Serialize)]
pub struct GraphRow {
    pub commit_id: String,
    pub lane: usize,
    pub parent_edges: Vec<GraphEdge>,
    pub ref_labels: Vec<String>,
    pub summary: String,
    pub author: String,
    pub date: i64,
    pub parents: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct GraphPage {
    pub rows: Vec<GraphRow>,
    pub total: usize,
    pub max_lane: usize,
    pub generation: u64,
}

#[derive(Debug, Serialize)]
pub struct RefsResponse {
    pub refs: Vec<RefInfo>,
    pub operation: Option<OperationInfo>,
    pub conflicted_files: usize,
    pub generation: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct OperationInfo {
    pub kind: &'static str,
    pub head_name: Option<String>,
    pub onto: Option<String>,
    pub step: Option<usize>,
    pub total_steps: Option<usize>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RepoChanged {
    pub repo_id: String,
    pub generation: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct RepoError {
    pub repo_id: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct JobStarted {
    pub job_id: JobId,
    pub repo_id: String,
    pub kind: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct JobFinished {
    pub job_id: JobId,
    pub repo_id: String,
    pub kind: String,
    pub label: String,
    pub ok: bool,
    pub message: String,
    pub stderr_tail: String,
    pub action: Option<JobAction>,
}

#[derive(Debug, Serialize)]
pub struct ChangedPath {
    pub path: String,
    pub status: &'static str,
    pub old_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CommitDetail {
    pub commit_id: String,
    pub message: String,
    pub summary: String,
    pub author: String,
    pub author_email: String,
    pub date: i64,
    pub parents: Vec<String>,
    pub changed_paths: Vec<ChangedPath>,
}
