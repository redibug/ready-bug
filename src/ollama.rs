//! Minimal Ollama client: model list + streaming chat over the local API.

use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::sync::mpsc::Sender;
use std::time::Duration;

#[derive(Clone, Debug, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub modified_at: String,
}

#[derive(Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<ModelInfo>,
}

pub fn base_url() -> String {
    let raw = std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".to_string());
    let raw = raw.trim().trim_end_matches('/').to_string();
    if raw.starts_with("http://") || raw.starts_with("https://") {
        raw
    } else {
        format!("http://{raw}")
    }
}

pub fn fetch_models() -> Result<Vec<ModelInfo>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(4))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(format!("{}/api/tags", base_url()))
        .send()
        .map_err(|_| "not detected".to_string())?;
    let tags: TagsResponse = resp.json().map_err(|e| e.to_string())?;
    Ok(tags.models)
}

#[derive(Clone, Debug, serde::Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// Reasoning text (Ollama's separate `thinking` field). Display-only:
    /// never sent back to the model in later turns.
    #[serde(default, skip_serializing)]
    pub thinking: String,
}

pub enum ChatEvent {
    Token(String),
    Thinking(String),
    Done,
    Error(String),
}

/// Blocking streaming chat; run on a thread. Sends tokens as they arrive.
pub fn stream_chat(model: String, messages: Vec<ChatMessage>, tx: Sender<ChatEvent>) {
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(ChatEvent::Error(e.to_string()));
            return;
        }
    };
    let resp = client
        .post(format!("{}/api/chat", base_url()))
        .json(&serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": true,
        }))
        .send();
    let resp = match resp {
        Ok(r) => r,
        Err(_) => {
            let _ = tx.send(ChatEvent::Error(
                "couldn't reach Ollama — is it running?".to_string(),
            ));
            return;
        }
    };
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        let hint = if body.contains("not found") {
            format!("model not found — pull it first (status {status})")
        } else {
            format!("Ollama said {status}: {}", body.chars().take(160).collect::<String>())
        };
        let _ = tx.send(ChatEvent::Error(hint));
        return;
    }
    let reader = BufReader::new(resp);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(thinking) = v["message"]["thinking"].as_str() {
            if !thinking.is_empty() {
                let _ = tx.send(ChatEvent::Thinking(thinking.to_string()));
            }
        }
        if let Some(content) = v["message"]["content"].as_str() {
            if !content.is_empty() {
                let _ = tx.send(ChatEvent::Token(content.to_string()));
            }
        }
        if v["done"].as_bool() == Some(true) {
            break;
        }
    }
    let _ = tx.send(ChatEvent::Done);
}

pub fn format_size(bytes: u64) -> String {
    if bytes == 0 {
        return "—".to_string();
    }
    let gb = bytes as f64 / 1_000_000_000.0;
    if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{:.0} MB", bytes as f64 / 1_000_000.0)
    }
}
