# Ready Bug 🐞

An **agent-first local AI command center** in the terminal.
Named agents with faces and personalities, running on local models —
with a learning-agent memory structure, Ollama model management, and
ComfyUI image/video generation, all in one Rust + ratatui TUI.

See [DESIGN.md](DESIGN.md) for the full design.

## Status: v0 — skeleton that chats

- Agents home screen (agents are folders of plain text — see below)
- Create an agent: name, model, avatar, persona/memory/lessons files
- Ollama auto-detection + installed model list
- Streaming chat; each session loads the agent's persona, memory, and
  lessons, plus a shared operator profile
- Avatars rendered with ratatui-image (Kitty / Sixel / iTerm2 where
  available, halfblocks fallback everywhere else)

Coming later (per DESIGN.md): Ollama model browser + pull/delete,
ComfyUI media library scan, generation, video.

## Build & run

You need Rust (via rustup) and, for Ollama features, a running Ollama.

### Windows (one-time)

1. Install rustup from <https://rustup.rs> — its installer offers to
   install the Visual Studio prerequisites (C++ build tools) for you;
   accept that.
2. Reopen your terminal, check: `cargo --version`

### Arch Linux (one-time)

```bash
sudo pacman -S rustup base-devel
rustup default stable
```

### After cloning (both)

```powershell
cargo run              # debug build — fine for testing
```

```powershell
cargo build --release  # optimized build
.\target\release\ready-bug.exe   # Windows
./target/release/ready-bug       # Linux/macOS
```

First `cargo run` downloads the libraries and compiles everything
(a few minutes); after that only changed code rebuilds.

**Tips**

- Run it in a graphics-capable terminal for full-quality avatars:
  Windows Terminal (≥ 1.22, Sixel), Kitty, WezTerm, Ghostty, Foot.
  In other terminals (GNOME Terminal, legacy conhost) avatars fall
  back to halfblocks — everything still works.
- Ollama must be running for model/chat features
  (`http://localhost:11434`). Without it, Ready Bug says so plainly
  and the rest of the app still works.
- Set `OLLAMA_HOST` to point at another Ollama, and
  `READYBUG_DATA_DIR` to move Ready Bug's data directory.

## Your agents

Data lives in the platform data dir (Windows:
`%LOCALAPPDATA%\ReadyBug`, Linux: `~/.local/share/readybug`) unless
`READYBUG_DATA_DIR` says otherwise:

```
operator.md            # shared profile of you, read by every agent
thinking.md            # house rules for how agents reason (below)
agents/<name>/
  agent.toml           # name, model, avatar, think mode
  avatar.png           # optional
  persona.md           # who this agent is
  memory.md            # what they've learned about you / your world
  lessons.md           # corrections — how not to get it wrong again
```

An agent is a folder. Copy the folder, copy the agent. Edit any file
in any text editor — Ready Bug reads them fresh each session.

## Thinking

Reasoning models stream their thought process live in chat (dimmed,
marked 💭) before the answer. Two controls:

- **`thinking.md`** sets the house style for reasoning — by default:
  think in first person, in character, and keep it as short as the
  question deserves. Edit it, or empty it to say nothing.
- **Think mode** per agent, cycled with `t` on the home screen or
  `ctrl+t` in chat, saved into `agent.toml`:
  - `on` — the model's default (full reasoning)
  - `auto` — simple messages (short, no hard-problem words like
    "why" or "debug") answer directly with no thinking phase;
    real questions think normally
  - `low` / `medium` / `high` — effort levels, for models that
    support them (e.g. gpt-oss)
  - `off` — never think; fastest replies, plainest answers
