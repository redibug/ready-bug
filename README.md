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
- Media screen (**g**): a headless ComfyUI daemon managed through
  comfy-cli in a dedicated workspace (`~/comfy/ReadyBug`, passed with
  `--workspace` on every command — your own comfy default is never
  touched). Install / launch / stop it from the app, link it to Comfy
  Desktop's shared model library (writes `extra_model_paths.yaml`),
  and browse every image/video model the scanner finds there.
  Duplicate files are flagged, never deleted — your files, your call.
- Mouse support: click an agent to select it, click it again to
  open its chat; the model/router pickers and the Media list work
  the same way. The wheel scrolls everywhere, and PageUp/PageDown
  scroll chat history and lists by the page. Every keyboard
  shortcut still works exactly as before.

Coming later (per DESIGN.md): Ollama model browser + pull/delete,
ComfyUI generation, video.

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
.\target\release\readybug.exe   # Windows
./target/release/readybug         # Linux/macOS
```

First `cargo run` downloads the libraries and compiles everything
(a few minutes); after that only changed code rebuilds.

## The `readybug` CLI (for games & scripts)

It's ONE binary with two front doors: bare `readybug` opens the
app above; any subcommand is the CLI — the shared setup surface so
other programs (our text adventure games, future apps) never carry
their own copy of the install logic:

```bash
readybug                  # the app (in a terminal)
readybug install          # prerequisites (Ollama, Python 3.13, comfy-cli)
                          # + ComfyUI into the shared workspace + link the
                          # Comfy Desktop shared model library
readybug doctor           # what's present / missing (--json for machines)
readybug comfy status     # workspace, daemon, models (--json too)
readybug comfy launch     # start the ComfyUI daemon
readybug comfy stop       # stop it
```

Games depend on the `ready_bug` library for runtime questions and
shell out to the CLI commands for setup.

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
agents/<name>/
  agent.toml           # name, model, avatar, models, router
  avatar.png           # optional
  persona.md           # who this agent is
  memory.md            # what they've learned about you / your world
  lessons.md           # corrections — how not to get it wrong again
```

An agent is a folder. Copy the folder, copy the agent. Edit any file
in any text editor — Ready Bug reads them fresh each session.

## Model routing

An agent can carry several models and let a small **router** model
pick which one answers each message — and whether that message
deserves step-by-step thinking, and at what effort. The agent's
persona, memory, and lessons never change; only the engine under
them does.

Two lines in `agent.toml` control it:

```toml
models = ["qwen3.5:9b", "gpt-oss:20b"]   # the router's choices, in priority order
router = "tev1:0.8b"                     # who decides (default if omitted)
```

- **No `models`** → the list is just the agent's `model`.
- **No `router`** → the default router, `tev1:0.8b`. Pull it once
  (`ollama pull tev1:0.8b`, ~0.8 GB) and routing works out of the box.
- **`router = "none"`** → no routing; the first model always answers.
- Pick the router in the app instead: press **R** on the home screen.
  Chat models and decision models both appear, labeled by type.

The router can be either kind of model:

- A **decision model** (Jev-style, e.g. `tev1`, `nimble`) answers
  typed questions — model choice, yes/no thinking, effort score —
  in a single fast pass, with probabilities. Needs Ollama 0.35+.
- A **chat model** is asked for the same verdict as JSON.

Either way: the current model is preferred — a switch has to win by
a real margin, so routing never thrashes between models. The router
sees a short window of recent conversation, not the agent's system
prompt. If the router is missing or fails, the chat quietly falls
back to the current model. Pressing **e** still swaps the home model
manually, and whichever model answered last becomes the current one.
