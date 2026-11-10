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
    /// The router picked this model + thinking setting for the exchange.
    Routed {
        model: String,
        think_label: String,
        router: String,
    },
    Done,
    Error(String),
}

/// How hard the answering model should think this turn.
#[derive(Clone, Debug)]
pub enum Think {
    /// Don't send a think field at all — the model's own default.
    Default,
    /// `think: false` — answer directly, no reasoning phase.
    Off,
    /// `think: "low" | "medium" | "high"` — for models with effort levels.
    Level(String),
}

pub fn think_label(think: &Think) -> String {
    match think {
        Think::Default => "on".to_string(),
        Think::Off => "off".to_string(),
        Think::Level(l) => l.clone(),
    }
}

fn client(timeout_secs: u64) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| e.to_string())
}

/// Capability strings for a model, from /api/show (e.g. "completion",
/// "decision", "thinking", "tools").
pub fn model_capabilities(model: &str) -> Result<Vec<String>, String> {
    let resp = client(10)?
        .post(format!("{}/api/show", base_url()))
        .json(&serde_json::json!({ "model": model }))
        .send()
        .map_err(|_| "couldn't reach Ollama".to_string())?;
    if !resp.status().is_success() {
        return Err(format!("model {model} not found"));
    }
    let v: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    Ok(v["capabilities"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| c.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default())
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RouterKind {
    /// Jev-style decision model, served by /v1/systemone.
    Decision,
    /// An ordinary chat model, asked for a JSON verdict.
    Text,
}

pub fn router_kind(model: &str) -> Result<RouterKind, String> {
    let caps = model_capabilities(model)?;
    if caps.iter().any(|c| c == "decision") {
        Ok(RouterKind::Decision)
    } else {
        Ok(RouterKind::Text)
    }
}

pub struct RouteDecision {
    pub model: String,
    pub think: Think,
}

/// Ask the router to pick a model + thinking setting for one message.
/// `state` carries recent history and the new message. Any failure is an
/// Err — callers fall back to the current model, never block the chat.
pub fn route(
    router: &str,
    kind: RouterKind,
    state: &str,
    models: &[String],
    current: &str,
) -> Result<RouteDecision, String> {
    match kind {
        RouterKind::Decision => route_via_decision(router, state, models, current),
        RouterKind::Text => route_via_text(router, state, models, current),
    }
}

fn route_via_decision(
    router: &str,
    state: &str,
    models: &[String],
    current: &str,
) -> Result<RouteDecision, String> {
    let mut criteria = serde_json::Map::new();
    for m in models {
        let desc = if m == current {
            "The current model. Prefer this one unless another model is clearly better for the new message."
        } else if m.to_lowercase().contains("coder") || m.to_lowercase().contains("code") {
            "Specialized for code, debugging, and technical work."
        } else {
            "General chat, writing, and reasoning."
        };
        criteria.insert(m.clone(), serde_json::json!(desc));
    }
    let body = serde_json::json!({
        "model": router,
        "state": { "context": state },
        "questions": {
            "model": {
                "type": "choice",
                "instructions": "Which model should answer the new message?",
                "criteria": criteria,
            },
            "needs_reasoning": {
                "type": "noul",
                "instructions": "Does the new message need step-by-step reasoning to answer well?",
            },
            "effort": {
                "type": "score",
                "instructions": "How much reasoning effort does the new message deserve?",
                "criteria": ["Low", "Medium", "High"],
            },
        },
    });
    let resp = client(30)?
        .post(format!("{}/v1/systemone", base_url()))
        .json(&body)
        .send()
        .map_err(|_| "couldn't reach Ollama".to_string())?;
    if resp.status().as_u16() == 404 {
        return Err("decision models need Ollama 0.35 or newer".to_string());
    }
    if !resp.status().is_success() {
        return Err(format!("router said {}", resp.status()));
    }
    let v: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    let answers = &v["answers"];

    // Model choice, with the current model preferred: a switch must win
    // by a probability margin (or overwhelming confidence).
    let model_ans = &answers["model"];
    let chosen = model_ans["choice"].as_str().unwrap_or(current);
    let chosen = if models.iter().any(|m| m == chosen) {
        chosen.to_string()
    } else {
        current.to_string()
    };
    let model = if chosen == current {
        current.to_string()
    } else {
        let probs = &model_ans["probabilities"];
        let p_chosen = probs[&chosen].as_f64();
        let p_current = probs[current].as_f64();
        let confidence = model_ans["confidence"].as_f64();
        let switch_ok = match (p_chosen, p_current) {
            (Some(pc), Some(pu)) => pc - pu >= 0.15 || confidence.unwrap_or(0.0) >= 0.75,
            _ => confidence.map(|c| c >= 0.5).unwrap_or(true),
        };
        if switch_ok {
            chosen
        } else {
            current.to_string()
        }
    };

    let noul = answers["needs_reasoning"]["noul"].as_f64().unwrap_or(0.0);
    let think = if noul >= 0.5 {
        let score = answers["effort"]["score"].as_f64().unwrap_or(0.5);
        let level = if score < 0.34 {
            "low"
        } else if score < 0.67 {
            "medium"
        } else {
            "high"
        };
        Think::Level(level.to_string())
    } else {
        Think::Off
    };
    Ok(RouteDecision { model, think })
}

fn route_via_text(
    router: &str,
    state: &str,
    models: &[String],
    current: &str,
) -> Result<RouteDecision, String> {
    let body = serde_json::json!({
        "model": router,
        "stream": false,
        "think": false,
        "format": {
            "type": "object",
            "properties": {
                "model": { "type": "string", "enum": models },
                "think": { "type": "boolean" },
                "effort": { "type": "string", "enum": ["low", "medium", "high"] },
            },
            "required": ["model", "think", "effort"],
        },
        "messages": [
            {
                "role": "system",
                "content": "You are a model router inside a chat app. Given the conversation state, pick which model should answer the new message, whether it deserves step-by-step reasoning (think), and how much effort (low, medium, high). Prefer the current model unless another listed model is clearly better for this message. Reply with JSON only.",
            },
            { "role": "user", "content": state },
        ],
    });
    let resp = client(60)?
        .post(format!("{}/api/chat", base_url()))
        .json(&body)
        .send()
        .map_err(|_| "couldn't reach Ollama".to_string())?;
    if !resp.status().is_success() {
        return Err(format!("router said {}", resp.status()));
    }
    let v: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    let content = v["message"]["content"].as_str().unwrap_or("");
    let verdict: serde_json::Value =
        serde_json::from_str(content).map_err(|_| "router reply wasn't JSON".to_string())?;
    let model = verdict["model"].as_str().unwrap_or(current);
    let model = if models.iter().any(|m| m == model) {
        model.to_string()
    } else {
        current.to_string()
    };
    let think = if verdict["think"].as_bool().unwrap_or(false) {
        Think::Level(
            verdict["effort"].as_str().unwrap_or("medium").to_string(),
        )
    } else {
        Think::Off
    };
    Ok(RouteDecision { model, think })
}

/// Blocking streaming chat; run on a thread. Sends tokens as they arrive.
pub fn stream_chat(model: String, messages: Vec<ChatMessage>, think: Think, tx: Sender<ChatEvent>) {
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
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
    });
    match think {
        Think::Default => {}
        Think::Off => body["think"] = serde_json::json!(false),
        Think::Level(level) => body["think"] = serde_json::json!(level),
    }
    let resp = client
        .post(format!("{}/api/chat", base_url()))
        .json(&body)
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
