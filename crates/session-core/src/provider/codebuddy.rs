//! CodeBuddy / WorkBuddy session provider.
//!
//! WorkBuddy (formerly CodeBuddy) stores its conversations under
//! `~/.workbuddy/projects/<encoded-cwd>/<session-uuid>.jsonl`. Each JSONL line
//! is a typed record:
//!   - `type: "message", role: "user"`      -> user text (`content[].input_text`)
//!   - `type: "message", role: "assistant"` -> assistant text (`content[].output_text`)
//!   - `type: "reasoning"`                  -> thinking (`rawContent[].reasoning_text`)
//!   - `type: "function_call"`              -> a tool/function call (`name`/`arguments`/`callId`)
//!   - `type: "function_call_result"`       -> tool output (`name`/`callId`/`output`)
//!   - `type: "ai-title"`                   -> AI generated session title (`aiTitle`)
//!
//! Every record carries `id`, `timestamp` (unix ms), `parentId`, `sessionId`,
//! `cwd` and `providerData` (model, trace id, token usage, ...).

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::models::message::{
    DisplayContentBlock, DisplayMessage, PaginatedMessages, RangeMessages,
};
use crate::models::project::ProjectEntry;
use crate::models::session::{SessionIndexEntry, SessionStatus};

/// Root directory (and layout) for WorkBuddy conversations.
const PROJECTS_DIR: &str = "projects";

/// Cap the rendered size of a tool call / tool result so huge payloads
/// (e.g. file contents) don't blow up the UI. Mirrors the Codex provider.
const MAX_ARGS_SIZE: usize = 10_000;
const MAX_OUTPUT_BLOCK_SIZE: usize = 30_000;

pub struct SessionMeta {
    pub id: String,
    pub cwd: Option<String>,
}

/// `~/.workbuddy/projects`
pub fn get_sessions_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".workbuddy").join(PROJECTS_DIR))
}

/// Convert a unix-millisecond timestamp into an RFC3339 string for display.
fn ms_to_iso(ts: u64) -> Option<String> {
    let secs = (ts / 1000) as i64;
    let nanos = ((ts % 1000) * 1_000_000) as u32;
    DateTime::from_timestamp(secs, nanos).map(|dt| dt.with_timezone(&Utc).to_rfc3339())
}

fn truncate_string(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let mut end = max_len;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}…[truncated]", &s[..end])
    }
}

fn text_content(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return (!text.trim().is_empty()).then(|| text.to_string());
    }

    let text = value
        .as_array()?
        .iter()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

/// Extract the human-readable text from a `function_call_result.output`
/// object, which may be `{type:"text", text}` / `{type:"json", json}` / raw.
fn tool_output_text(output: &Value) -> String {
    match output {
        Value::String(s) => s.clone(),
        Value::Object(_) => {
            if let Some(text) = output.get("text").and_then(Value::as_str) {
                text.to_string()
            } else if let Some(json) = output.get("json") {
                serde_json::to_string_pretty(json).unwrap_or_else(|_| output.to_string())
            } else {
                serde_json::to_string_pretty(output).unwrap_or_else(|_| output.to_string())
            }
        }
        other => other.to_string(),
    }
}

fn display_message_from_row(row: &Value) -> Option<DisplayMessage> {
    let row_type = row.get("type")?.as_str()?;
    let timestamp = row
        .get("timestamp")
        .and_then(Value::as_u64)
        .and_then(ms_to_iso);
    let model = row
        .get("providerData")
        .and_then(|pd| pd.get("model"))
        .and_then(Value::as_str)
        .map(ToString::to_string);

    match row_type {
        // ── user message ──
        "message" if row.get("role").and_then(Value::as_str) == Some("user") => {
            let content = row.get("content").and_then(text_content)?;
            Some(DisplayMessage {
                uuid: row.get("id").and_then(Value::as_str).map(ToString::to_string),
                parent_uuid: row
                    .get("parentId")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                role: "user".to_string(),
                timestamp,
                model: None,
                content: vec![DisplayContentBlock::Text { text: content }],
            })
        }
        // ── assistant message ──
        "message" if row.get("role").and_then(Value::as_str) == Some("assistant") => {
            let content = row.get("content").and_then(text_content)?;
            Some(DisplayMessage {
                uuid: row.get("id").and_then(Value::as_str).map(ToString::to_string),
                parent_uuid: row
                    .get("parentId")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                role: "assistant".to_string(),
                timestamp,
                model,
                content: vec![DisplayContentBlock::Text { text: content }],
            })
        }
        // ── reasoning / thinking ──
        "reasoning" => {
            let text = row
                .get("rawContent")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|b| b.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .filter(|t| !t.trim().is_empty())
                .or_else(|| {
                    row.get("content")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|b| b.get("text").and_then(Value::as_str))
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .filter(|t| !t.trim().is_empty())
                })
                .or_else(|| row.get("summary").and_then(Value::as_str).map(ToString::to_string));
            let text = text?;
            if text.trim().is_empty() {
                return None;
            }
            Some(DisplayMessage {
                uuid: row.get("id").and_then(Value::as_str).map(ToString::to_string),
                parent_uuid: row
                    .get("parentId")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                role: "assistant".to_string(),
                timestamp,
                model,
                content: vec![DisplayContentBlock::Reasoning { text }],
            })
        }
        // ── function / tool call ──
        "function_call" => {
            let name = row
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let arguments = row
                .get("arguments")
                .map(|v| match v {
                    Value::String(s) => {
                        if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                            serde_json::to_string_pretty(&parsed).unwrap_or_else(|_| s.clone())
                        } else {
                            s.clone()
                        }
                    }
                    other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
                })
                .filter(|s| !s.trim().is_empty())
                .or_else(|| {
                    row.get("argumentsDisplayText")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                })
                .unwrap_or_default();
            let call_id = row
                .get("callId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Some(DisplayMessage {
                uuid: row.get("id").and_then(Value::as_str).map(ToString::to_string),
                parent_uuid: row
                    .get("parentId")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                role: "assistant".to_string(),
                timestamp,
                model,
                content: vec![DisplayContentBlock::FunctionCall {
                    name,
                    arguments: truncate_string(&arguments, MAX_ARGS_SIZE),
                    call_id,
                }],
            })
        }
        // ── function / tool result ──
        "function_call_result" => {
            let call_id = row
                .get("callId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let output = row.get("output").map(tool_output_text).unwrap_or_default();
            Some(DisplayMessage {
                uuid: row.get("id").and_then(Value::as_str).map(ToString::to_string),
                parent_uuid: row
                    .get("parentId")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                role: "tool".to_string(),
                timestamp,
                model: None,
                content: vec![DisplayContentBlock::FunctionCallOutput {
                    call_id,
                    output: truncate_string(&output, MAX_OUTPUT_BLOCK_SIZE),
                }],
            })
        }
        _ => None,
    }
}

pub fn parse_all_messages(path: &Path) -> Result<Vec<DisplayMessage>, String> {
    let file =
        fs::File::open(path).map_err(|error| format!("Failed to open CodeBuddy session: {error}"))?;

    Ok(BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .filter_map(|row| display_message_from_row(&row))
        .collect())
}

fn text_message_count(messages: &[DisplayMessage]) -> u32 {
    messages
        .iter()
        .filter(|message| {
            message
                .content
                .iter()
                .any(|block| matches!(block, DisplayContentBlock::Text { .. }))
        })
        .count() as u32
}

pub fn count_messages(path: &Path) -> u32 {
    parse_all_messages(path)
        .map(|messages| text_message_count(&messages))
        .unwrap_or(0)
}

/// Read the real cwd for a project directory from any of its session records.
fn project_cwd(dir: &Path) -> Option<String> {
    let Ok(sessions) = fs::read_dir(dir) else {
        return None;
    };
    for entry in sessions.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if let Ok(file) = fs::File::open(&path) {
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                if let Ok(row) = serde_json::from_str::<Value>(&line) {
                    if let Some(cwd) = row.get("cwd").and_then(Value::as_str) {
                        if !cwd.trim().is_empty() {
                            return Some(cwd.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

pub fn extract_session_meta(path: &Path) -> Option<SessionMeta> {
    let id = path.file_stem()?.to_string_lossy().into_owned();
    let cwd = {
        let mut found = None;
        if let Ok(file) = fs::File::open(path) {
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                if let Ok(row) = serde_json::from_str::<Value>(&line) {
                    if let Some(cwd) = row.get("cwd").and_then(Value::as_str) {
                        if !cwd.trim().is_empty() {
                            found = Some(cwd.to_string());
                            break;
                        }
                    }
                }
            }
        }
        found
    };
    Some(SessionMeta { id, cwd })
}

fn session_entry(path: &Path) -> Option<SessionIndexEntry> {
    let session_id = path.file_stem()?.to_string_lossy().into_owned();
    let messages = parse_all_messages(path).unwrap_or_default();

    // created / modified from first / last records' timestamps.
    let (created, modified) = {
        let mut created = None;
        let mut modified = None;
        if let Ok(file) = fs::File::open(path) {
            let mut first = true;
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                if let Ok(row) = serde_json::from_str::<Value>(&line) {
                    if let Some(ts) = row.get("timestamp").and_then(Value::as_u64) {
                        if first {
                            created = ms_to_iso(ts);
                            first = false;
                        }
                        modified = ms_to_iso(ts);
                    }
                }
            }
        }
        (created, modified)
    };

    // AI-generated title.
    let thread_name = {
        let mut title = None;
        if let Ok(file) = fs::File::open(path) {
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                if let Ok(row) = serde_json::from_str::<Value>(&line) {
                    if row.get("type").and_then(Value::as_str) == Some("ai-title") {
                        if let Some(t) = row.get("aiTitle").and_then(Value::as_str) {
                            if !t.trim().is_empty() {
                                title = Some(t.to_string());
                                break;
                            }
                        }
                    }
                }
            }
        }
        title
    };

    let cwd = project_cwd(path.parent()?);
    let model_provider = messages
        .iter()
        .find_map(|m| m.model.clone())
        .or_else(|| {
            if let Ok(file) = fs::File::open(path) {
                for line in BufReader::new(file).lines().map_while(Result::ok) {
                    if let Ok(row) = serde_json::from_str::<Value>(&line) {
                        if let Some(model) =
                            row.get("providerData").and_then(|pd| pd.get("model")).and_then(Value::as_str)
                        {
                            return Some(model.to_string());
                        }
                    }
                }
            }
            None
        });

    let message_count = messages.len() as u32;
    let first_prompt = messages.iter().find_map(|message| {
        if message.role != "user" {
            return None;
        }
        message.content.iter().find_map(|block| match block {
            DisplayContentBlock::Text { text } => Some(text.chars().take(200).collect()),
            _ => None,
        })
    });

    Some(SessionIndexEntry {
        source: "codebuddy".to_string(),
        session_id,
        file_path: path.to_string_lossy().into_owned(),
        first_prompt,
        thread_name,
        message_count,
        created,
        modified,
        git_branch: None,
        project_path: cwd.clone(),
        is_sidechain: None,
        cwd,
        model_provider,
        cli_version: None,
        alias: None,
        tags: None,
        status: if message_count == 0 {
            SessionStatus::Empty
        } else {
            SessionStatus::Valid
        },
    })
}

pub fn get_projects() -> Result<Vec<ProjectEntry>, String> {
    let Some(root) = get_sessions_dir() else {
        return Ok(Vec::new());
    };
    let Ok(projects) = fs::read_dir(root) else {
        return Ok(Vec::new());
    };

    let mut entries: Vec<ProjectEntry> = Vec::new();
    for project in projects.flatten() {
        let dir = project.path();
        if !dir.is_dir() {
            continue;
        }
        let project_id = dir.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        if project_id.is_empty() {
            continue;
        }

        let mut sessions: Vec<SessionIndexEntry> = Vec::new();
        if let Ok(sessions_dir) = fs::read_dir(&dir) {
            for entry in sessions_dir.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    if let Some(session) = session_entry(&path) {
                        sessions.push(session);
                    }
                }
            }
        }

        let cwd = project_cwd(&dir);
        let display_path = cwd.clone().unwrap_or_else(|| project_id.clone());
        let short_name = cwd
            .as_ref()
            .and_then(|c| Path::new(c).file_name())
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| project_id.clone());

        let session_count = sessions.len();
        let last_modified = sessions
            .iter()
            .filter_map(|s| s.modified.clone())
            .max();

        entries.push(ProjectEntry {
            source: "codebuddy".to_string(),
            id: project_id.clone(),
            display_path,
            short_name,
            session_count,
            last_modified,
            model_provider: None,
            alias: None,
            path_exists: cwd.as_deref().map(Path::new).map(|p| p.exists()).unwrap_or(false),
            is_virtual: cwd.is_none(),
        });
    }

    // Sort by last activity, newest first.
    entries.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));
    Ok(entries)
}

pub fn refresh_projects_cache() -> Result<Vec<ProjectEntry>, String> {
    get_projects()
}

pub fn rebuild_projects_cache() -> Result<Vec<ProjectEntry>, String> {
    get_projects()
}

/// Keep the common lifecycle hook so callers don't need a CodeBuddy-only branch.
pub fn invalidate_sessions_cache() {}

fn sessions_for_project(project_id: &str) -> Vec<SessionIndexEntry> {
    let Some(root) = get_sessions_dir() else {
        return Vec::new();
    };
    let dir = root.join(project_id);
    let mut sessions: Vec<_> = Vec::new();
    if let Ok(sessions_dir) = fs::read_dir(&dir) {
        for entry in sessions_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                if let Some(session) = session_entry(&path) {
                    sessions.push(session);
                }
            }
        }
    }
    sessions.sort_by(|left, right| right.modified.cmp(&left.modified));
    sessions
}

pub fn get_sessions(project_id: &str) -> Result<Vec<SessionIndexEntry>, String> {
    Ok(sessions_for_project(project_id))
}

pub fn refresh_sessions_cache(project_id: &str) -> Result<Vec<SessionIndexEntry>, String> {
    Ok(sessions_for_project(project_id))
}

pub fn get_invalid_sessions(project_id: &str) -> Result<Vec<SessionIndexEntry>, String> {
    Ok(sessions_for_project(project_id)
        .into_iter()
        .filter(|session| session.status != SessionStatus::Valid)
        .collect())
}

pub fn delete_project(project_id: &str) -> Result<super::claude::DeleteResult, String> {
    if project_id.is_empty() {
        return Err("Invalid project id".to_string());
    }
    let Some(root) = get_sessions_dir() else {
        return Err("Could not find CodeBuddy projects directory".to_string());
    };
    let dir = root.join(project_id);
    if !dir.exists() {
        return Err(format!("Project not found: {}", project_id));
    }

    // Count jsonl sessions before moving the whole project folder to the recycle bin.
    let sessions_deleted = fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jsonl"))
                .count()
        })
        .unwrap_or(0);

    let moved = crate::recyclebin::move_to_recyclebin(
        &dir,
        "project",
        "ManualDelete",
        "codebuddy",
        project_id,
        None,
        Some(project_id.to_string()),
    )
    .is_ok();

    Ok(super::claude::DeleteResult {
        sessions_deleted: if moved { sessions_deleted } else { 0 },
        config_cleaned: false,
        bookmarks_removed: 0,
    })
}

pub fn parse_session_messages(
    path: &Path,
    page: usize,
    page_size: usize,
    from_end: bool,
) -> Result<PaginatedMessages, String> {
    let all = parse_all_messages(path)?;
    let total = all.len();
    let (start, end) = if from_end {
        (
            total.saturating_sub((page + 1).saturating_mul(page_size)),
            total.saturating_sub(page.saturating_mul(page_size)),
        )
    } else {
        let start = page.saturating_mul(page_size).min(total);
        (start, start.saturating_add(page_size).min(total))
    };

    Ok(PaginatedMessages {
        messages: all[start..end].to_vec(),
        total,
        page,
        page_size,
        has_more: if from_end { start > 0 } else { end < total },
    })
}

pub fn parse_messages_range(
    path: &Path,
    start: usize,
    end: usize,
) -> Result<RangeMessages, String> {
    let all = parse_all_messages(path)?;
    let total = all.len();
    let start = start.min(total);
    let end = end.min(total).max(start);
    Ok(RangeMessages {
        messages: all[start..end].to_vec(),
        total,
        start,
        end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_visible_history_and_paginates_from_end() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "ai-session-viewer-codebuddy-{}-{unique}.jsonl",
            std::process::id()
        ));
        let rows = [
            serde_json::json!({"type":"system","content":"hidden"}),
            serde_json::json!({"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}),
            serde_json::json!({"type":"reasoning","rawContent":[{"type":"reasoning_text","text":"thinking"}]}),
            serde_json::json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"world"}],"providerData":{"model":"deepseek-v4-pro"}}),
            serde_json::json!({"type":"function_call","name":"Bash","arguments":"{\"command\":\"ls\"}","callId":"call_1"}),
            serde_json::json!({"type":"function_call_result","name":"Bash","callId":"call_1","output":{"type":"text","text":"file.txt"}}),
        ];
        fs::write(
            &path,
            rows.iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();

        assert_eq!(count_messages(&path), 2);
        let page = parse_session_messages(&path, 0, 4, true).unwrap();
        assert_eq!(page.total, 5);
        assert_eq!(page.messages.len(), 4);
        assert!(page.has_more);

        fs::remove_file(path).unwrap();
    }
}
