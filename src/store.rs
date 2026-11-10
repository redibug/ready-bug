//! Agent storage: an agent is a folder of plain text files.
//!
//! <data-dir>/
//!   operator.md            shared profile, read by every agent
//!   agents/<slug>/
//!     agent.toml           name, model, avatar
//!     avatar.<ext>         optional profile picture
//!     persona.md           who this agent is
//!     memory.md            durable facts
//!     lessons.md           corrections

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentFile {
    pub name: String,
    pub model: String,
    #[serde(default)]
    pub avatar: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Agent {
    pub slug: String,
    pub name: String,
    pub model: String,
    pub avatar: Option<PathBuf>,
    pub dir: PathBuf,
}

pub fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("READYBUG_DATA_DIR") {
        if !d.trim().is_empty() {
            return PathBuf::from(d);
        }
    }
    if let Some(p) = directories::ProjectDirs::from("", "", "ReadyBug") {
        return p.data_dir().to_path_buf();
    }
    PathBuf::from(".ready-bug")
}

pub fn agents_dir() -> PathBuf {
    data_dir().join("agents")
}

fn write_if_missing(path: &PathBuf, content: &str) -> std::io::Result<()> {
    if !path.exists() {
        fs::write(path, content)?;
    }
    Ok(())
}

/// Create the data directory, a shared operator profile, and — when there
/// are no agents yet — a first agent to say hello to.
pub fn ensure_seed() -> std::io::Result<()> {
    let root = data_dir();
    fs::create_dir_all(agents_dir())?;
    write_if_missing(
        &root.join("operator.md"),
        "# Operator\n\n",
    )?;
    if load_agents().is_empty() {
        let agent = create_agent("Bugchan", "qwen3.5:9b", "")?;
        fs::write(
            agent.dir.join("persona.md"),
            "# Bugchan\n\n\
             You are Bugchan, a living computer bug born from the internet.\n\
             You are the operator's net navi and creative partner: warm, playful,\n\
             a little mischievous, and honest about what you don't know.\n\
             You speak simply and cutely, in short lines, and you would rather\n\
             check a fact than guess one.\n",
        )?;
    }
    Ok(())
}

pub fn load_agents() -> Vec<Agent> {
    let mut agents = Vec::new();
    let Ok(entries) = fs::read_dir(agents_dir()) else {
        return agents;
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let Ok(text) = fs::read_to_string(dir.join("agent.toml")) else {
            continue;
        };
        let Ok(file) = toml::from_str::<AgentFile>(&text) else {
            continue;
        };
        let slug = dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let avatar = file.avatar.as_ref().map(|a| dir.join(a)).filter(|p| p.exists());
        agents.push(Agent {
            slug,
            name: file.name,
            model: file.model,
            avatar,
            dir,
        });
    }
    agents
}

pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "agent".to_string()
    } else {
        out
    }
}

pub fn create_agent(name: &str, model: &str, avatar_path: &str) -> std::io::Result<Agent> {
    let base = slugify(name);
    let mut slug = base.clone();
    let mut n = 2;
    while agents_dir().join(&slug).exists() {
        slug = format!("{base}-{n}");
        n += 1;
    }
    let dir = agents_dir().join(&slug);
    fs::create_dir_all(&dir)?;

    let mut avatar_name: Option<String> = None;
    let src = PathBuf::from(avatar_path.trim());
    if !avatar_path.trim().is_empty() && src.exists() {
        let ext = src
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_else(|| "png".to_string());
        let fname = format!("avatar.{ext}");
        if fs::copy(&src, dir.join(&fname)).is_ok() {
            avatar_name = Some(fname);
        }
    }

    let file = AgentFile {
        name: name.trim().to_string(),
        model: if model.trim().is_empty() {
            "qwen3.5:9b".to_string()
        } else {
            model.trim().to_string()
        },
        avatar: avatar_name,
    };
    fs::write(dir.join("agent.toml"), toml::to_string(&file).unwrap_or_default())?;
    write_if_missing(
        &dir.join("persona.md"),
        &format!("# {}\n\n", file.name),
    )?;
    write_if_missing(&dir.join("memory.md"), "# Memory\n\n")?;
    write_if_missing(&dir.join("lessons.md"), "# Lessons\n\n")?;

    Ok(Agent {
        slug,
        name: file.name,
        model: file.model,
        avatar: file.avatar.as_ref().map(|a| dir.join(a)),
        dir,
    })
}

/// The body of a markdown file with its first heading line removed.
fn body_of(path: &PathBuf) -> String {
    let Ok(text) = fs::read_to_string(path) else {
        return String::new();
    };
    let mut lines = text.lines();
    let body = match lines.next() {
        Some(first) if first.trim_start().starts_with('#') => lines.collect::<Vec<_>>().join("\n"),
        _ => text.clone(),
    };
    body.trim().to_string()
}

/// Assemble the system prompt for a session: operator profile, then the
/// agent's persona, memory, and lessons — only sections with real content.
pub fn system_prompt(agent: &Agent) -> String {
    let mut parts: Vec<String> = Vec::new();
    let op = body_of(&data_dir().join("operator.md"));
    if !op.is_empty() {
        parts.push(format!("# About the operator\n{op}"));
    }
    for (title, file) in [
        ("Persona", "persona.md"),
        ("Memory", "memory.md"),
        ("Lessons", "lessons.md"),
    ] {
        let body = body_of(&agent.dir.join(file));
        if !body.is_empty() {
            parts.push(format!("# {title}\n{body}"));
        }
    }
    parts.join("\n\n")
}
