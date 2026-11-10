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
    /// Thinking mode: on (default) / auto / low / medium / high / off.
    #[serde(default)]
    pub think: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Agent {
    pub slug: String,
    pub name: String,
    pub model: String,
    pub avatar: Option<PathBuf>,
    /// Avatar file name relative to the agent folder (for rewriting agent.toml).
    pub avatar_file: Option<String>,
    pub think: Option<String>,
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
    write_if_missing(
        &root.join("thinking.md"),
        "# How to think\n\n\
         Think in first person, in your own voice and character.\n\
         Your reasoning is your inner monologue, not a report about\n\
         yourself written by a stranger.\n\n\
         Keep your thinking as short as the question deserves.\n\
         Greetings and simple questions need a sentence or two at most.\n\
         Save long reasoning for problems that are actually hard.\n",
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
        // agent.toml may name the avatar; otherwise any avatar.<ext> file
        // dropped into the folder counts — that's what the UI tells users.
        let avatar = file
            .avatar
            .as_ref()
            .map(|a| dir.join(a))
            .filter(|p| p.exists())
            .or_else(|| {
                ["avatar.png", "avatar.jpg", "avatar.jpeg", "avatar.webp", "avatar.gif", "avatar.bmp"]
                    .iter()
                    .map(|n| dir.join(n))
                    .find(|p| p.exists())
            });
        let avatar_file = avatar
            .as_ref()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
        agents.push(Agent {
            slug,
            name: file.name,
            model: file.model,
            avatar,
            avatar_file,
            think: file.think,
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
        think: None,
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
        name: file.name.clone(),
        model: file.model.clone(),
        avatar: file.avatar.as_ref().map(|a| dir.join(a)),
        avatar_file: file.avatar.clone(),
        think: None,
        dir,
    })
}

/// Rewrite an agent's agent.toml from its current in-memory config.
pub fn save_config(agent: &Agent) -> std::io::Result<()> {
    let file = AgentFile {
        name: agent.name.clone(),
        model: agent.model.clone(),
        avatar: agent.avatar_file.clone(),
        think: agent.think.clone(),
    };
    fs::write(
        agent.dir.join("agent.toml"),
        toml::to_string(&file).unwrap_or_default(),
    )
}

/// Persist a new model choice for an agent.
pub fn set_model(agent: &Agent, model: &str) -> std::io::Result<()> {
    let mut updated = agent.clone();
    updated.model = model.to_string();
    save_config(&updated)
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
    let persona = body_of(&agent.dir.join("persona.md"));
    if !persona.is_empty() {
        parts.push(format!("# Persona\n{persona}"));
    }
    // Shared reasoning house rules (data-dir thinking.md, user-editable;
    // an empty or deleted file simply drops this section).
    let thinking = body_of(&data_dir().join("thinking.md"));
    if !thinking.is_empty() {
        parts.push(format!("# How to think\n{thinking}"));
    }
    for (title, file) in [("Memory", "memory.md"), ("Lessons", "lessons.md")] {
        let body = body_of(&agent.dir.join(file));
        if !body.is_empty() {
            parts.push(format!("# {title}\n{body}"));
        }
    }
    parts.join("\n\n")
}
