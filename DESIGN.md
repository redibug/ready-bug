# Ready Bug — AI Command Center

*Design v0.1 — 2026-11-10. Mel + Bugchan.*

A Rust terminal app where **agents come first** and models are plumbing.
Named agents with faces and personalities, running on local models,
with a learning-agent memory structure, Ollama model management,
and ComfyUI image/video generation — all in one ratatui TUI.

## Principles

1. **Agent-first.** The home screen is your agents, not a model list.
   Models, media, and settings live underneath as support tabs.
2. **Local-first, files-you-own.** All state is readable plain text
   (TOML / Markdown / JSON) in one data directory. No database, no cloud
   account, no memory on someone else's server. If Ready Bug vanished,
   you'd still have every agent, persona, and lesson as files.
3. **Learning by structure, not vibes.** Agents improve through explicit
   memory files the harness loads every session and tools that file
   corrections the turn they happen (see Learning).
4. **Text stays truth.** Images (avatars, previews, results) are
   decoration and convenience. All status the user needs also exists as
   text, so the app stays fully usable in halfblocks terminals — and
   fully debuggable from plain terminal captures.
5. **Detect, don't ask.** If Ollama, ComfyUI, or a model folder exists on
   the machine, Ready Bug finds it. Manual paths are an override, never
   a requirement.

## Data layout

```
<data-dir>/                     # e.g. ~/.local/share/ready-bug (Linux)
  config.toml                   # endpoints, search dirs, comfy roots, ui prefs
  agents/
    bugchan/
      agent.toml                # name, model, avatar, options
      avatar.png
      persona.md                # who this agent is (SOUL)
      memory.md                 # facts about the operator/world
      lessons.md                # corrections + how-not-to-again
      sessions/                 # chat history, JSONL per session
    <other-agent>/
      ...
  operator.md                   # shared operator profile (USER.md equivalent),
                                # read by every agent
  media-index.json              # generated: scanned image/video model index
  catalog-cache.json            # generated: Ollama library browse cache
  workflows/                    # bundled + user-imported ComfyUI API workflows
  outputs/                      # generated images/videos fetched from ComfyUI
```

Agent identity is a folder. Copy the folder, copy the agent.

## Screens

- **Agents (home)** — agent cards: avatar (ratatui-image), name, model
  badge, last-chatted. New / edit / delete agent.
- **Chat** — avatar header, streaming reply, visible tool calls.
  Per-agent sessions, resumable.
- **Models** — two panes: *Installed* and *Browse* (Ollama).
- **Media** — *Generate* (prompt + workflow + model slots) and
  *Library* (detected image/video models by category).
- **Settings** — Ollama host, ComfyUI host, extra model search
  directories, Comfy install roots, terminal graphics protocol override.

## Agents & the learning structure

Each agent is defined by `agent.toml` + `persona.md`, and carries the
same three-file learning structure we built for the OpenCode pack:

- `persona.md` — stable identity and voice.
- `memory.md` — durable facts: operator, projects, preferences.
- `lessons.md` — corrections and mistakes, each with what to do instead.

Harness behaviour (this is where being our own app beats a config pack):

1. **Load on start.** Every session begins with operator.md + persona +
   memory + lessons injected into context, in that order.
2. **File with tools, not memory-of-the-model.** The agent gets explicit
   tools — `remember(fact)`, `learn(lesson)`, `correct(old, new)` — that
   edit the files directly (append, or replace-in-place for corrections).
   Filing is a tool call the harness can confirm, not a promise.
3. **End-of-session sweep.** On exit/switch, the harness asks the model
   one structured question — "anything from this session belong in
   memory or lessons?" — and shows the operator a diff to approve.
   (Approval UI: later stage; v0 logs proposals to `lessons.proposed.md`.)
4. **Honest limit, kept.** Filing still depends on the model noticing.
   Tools + sweep make it far more reliable than instructions alone,
   but no harness can guarantee a model learns. The files stay human-
   editable, so the operator can always teach by typing.

Model capability gating: agents on models with the `tools` capability
get the full tool set. Models without it run chat-only, and the UI says
so plainly on the agent card.

Agent tools, staged: memory tools (above) → `generate_image` (Media) →
project file read/write → shell commands behind an approval modal
(every command shown before it runs, like an approval card).

## Ollama integration (verified against the official API)

Base URL `http://localhost:11434` (configurable, respects OLLAMA_HOST).
Ollama running? Probe `GET /api/tags` (doubles as the installed list).

| Need | Endpoint |
| --- | --- |
| Installed models | `GET /api/tags` → name, size, modified |
| Model details | `POST /api/show` → family, parameter size, quant, capabilities |
| Loaded in VRAM now | `GET /api/ps` |
| Chat (agents) | `POST /api/chat`, streamed |
| One-shot generate | `POST /api/generate`, streamed |
| Install | `POST /api/pull` — NDJSON progress stream → progress bar |
| Uninstall | `DELETE /api/delete` — behind a confirm modal showing GB freed |

### Model browser

Ollama has **no official catalog API**. The library site is
server-rendered and parseable, so the browser is:

- Search: `GET https://ollama.com/search?q=<query>` → name, blurb, pull
  count, tag count, capability chips (vision / tools / thinking /
  embedding), size range.
- Detail: `GET https://ollama.com/library/<name>` → every tag with its
  size; pick a tag → confirm → `/api/pull` with a live progress screen.
- Results cached to `catalog-cache.json` with a timestamp; offline =
  browser greyed out with a note, everything else keeps working.
- **Fit badges measure two numbers, not one** (Mel, 2026-11-10): total
  size and *active* size. Dense and MoE models spill very differently —
  a dense model reads every weight for every token, so whatever spills
  out of VRAM is paid at RAM speed on every token; an MoE model only
  wakes a few experts per token, so a total size far over VRAM can
  still run fast as long as the *active* working set fits (or nearly).
  - Per model: `total` (disk + must fit in VRAM+RAM to load at all),
    `active` (per-token working set estimate: shared/non-expert weights
    + active experts' share of expert weights, at the tag's quant),
    plus a headroom reserve for KV cache at the configured context.
  - Dense: active = total. One-axis verdict → `fits VRAM` /
    `spills — slow per token` / `won't load`.
  - MoE: two verdicts — **speed** from active vs VRAM (`fast from RAM`,
    `active spills too — expect slowdowns`) and **residence** from
    total vs VRAM+RAM. Displayed like:
    `MoE · 30B total / ~3B active — fast despite its size`.
    Worked examples on a 6 GB card: gpt-oss:20b (~3.6B active) and
    qwen3-coder:30b-a3b both badge *fast*; a dense 32B badges *crawl*.
  - Where active comes from: installed models — `/api/show` GGUF
    metadata (`<arch>.expert_count`, `<arch>.expert_used_count`;
    expert_count > 0 is also the MoE detector). Browser models —
    the `-a3b`-style name suffix and "active parameters" text on the
    library page, marked as an estimate (`~`). Estimates are labeled
    estimates; the badge never pretends to be a benchmark.
- Fragility note: HTML parsing can break when the site changes. Parser
  is isolated in one module with fixture tests; a breakage degrades the
  browser only, never the app.

## Media: image & video generation via ComfyUI

**Engine arrangement (decided 2026-11-11, daemon layer SHIPPED):**
the server is a comfy-cli-managed headless install in a dedicated
workspace — `~/comfy/ReadyBug` (Windows `C:/Users/<user>/comfy/ReadyBug`)
— shared by Ready Bug, the text adventure games, and future apps.
Every comfy invocation from our code passes `--workspace` explicitly;
the user's own comfy-cli default is NEVER changed (no `set-default`).
The Media screen (**g**) detects comfy-cli, installs/launches/stops
the daemon (`comfy launch --background` / `comfy stop`), writes the
`extra_model_paths.yaml` linking the install to Comfy Desktop's shared
library. NOTE (learned live 2026-11-12): current comfy-cli (1.26)
installs ComfyUI directly INTO the workspace root (`main.py` at the
root, plus its own `comfy/` subfolder); older docs/versions used a
`ComfyUI/` subfolder. The app DETECTS the install root by probing for
`main.py` (root, then `ComfyUI/`, then `comfy/ComfyUI/`) rather than
assuming a layout.
It lists scanned models
with duplicate warnings — dedupe is the user's responsibility, the
app only warns. Comfy Desktop remains the tool for visually building
workflows. Generation (below) drives whichever server answers :8188.

Ready Bug does **not** reimplement diffusion. It drives a ComfyUI
server (Desktop or standalone) through its official API:

| Need | Endpoint |
| --- | --- |
| Queue a workflow | `POST /prompt` (API-format workflow JSON) |
| Live progress + previews | WebSocket `/ws` |
| Results | `GET /history/{prompt_id}` |
| Fetch image bytes | `GET /view?filename=…` |
| Server / model info | `GET /object_info` (also validates slot choices) |

Generation flow: pick a workflow template → its declared **slots**
(diffusion model / checkpoint, text encoder, VAE, LoRA…) are filled from
the detected model index → set prompt/seed/size → queue → live preview
frames stream into a ratatui-image panel → result lands in `outputs/`
and is displayed in-app. Video results show their first frame in-app
with the file path + "open externally".

Bundled templates (staged): txt2img via checkpoint; txt2img via
diffusion-model + text-encoder + VAE (FLUX/SD3-style); txt2vid for
three-file video models (Wan / H3-style). Users can import any
API-format workflow JSON and map its model inputs to slots.

Agents can call generation too: `generate_image(prompt)` runs the
agent's default workflow and shows the result in chat. Generation is an
agent capability first, a Media tab second.

### Media model detection

Scan roots, in order (all paths probed; missing ones skipped silently):

1. **Comfy Desktop shared library**
   - Windows: `%LOCALAPPDATA%\Comfy-Desktop\ComfyUI-Shared\models`
   - macOS / Linux: `~/ComfyUI-Shared/models`
   - Older/other layouts probed too: `%APPDATA%\ComfyUI`,
     `~/.local/share/ComfyUI/models`, `~/ComfyUI-Installs/*/ComfyUI/models`
2. **Comfy config files** — `shared_model_paths.yaml` and
   `extra_model_paths.yaml` wherever found near the above: parse
   `base_path` + per-category subpaths. This also imports A1111 /
   Stability Matrix style folder names (`StableDiffusion`, `Lora`, …)
   through the same mapping ComfyUI itself uses.
3. **Standalone ComfyUI installs** added in Settings → their `models/`
   folder + their own `extra_model_paths.yaml`.
4. **User-defined directories** (Settings): each is either a *root*
   (contains category subfolders) or a *single category* folder
   (user picks which). This is the feature asked for: define new
   directories for Ready Bug to search.

Classification = the category subfolder name, ComfyUI's own taxonomy:
`checkpoints`, `diffusion_models` (alias `unet`), `text_encoders`
(alias `clip`), `clip_vision`, `vae`, `vae_approx`, `loras`,
`controlnet`, `upscale_models`, `embeddings`, `ipadapter`, and the rest
of Comfy's folder list. Extensions: `.safetensors .gguf .ckpt .pt .pth
.bin .sft`. Symlinks followed (shared libraries use them). Duplicates
deduped by canonical path.

Image vs video honesty: a file's *folder* says what kind of model it is;
it cannot say image vs video — video models are diffusion models used
by video workflows. So video-capable models get a **name-based hint**
(known families: Wan, Hunyuan Video, LTXV, Mochi, CogVideo, MiniMax/H3,
SVD…) shown as `video (by name)`, and workflow slot-matching is the
final word. No fake certainty.

Index: one scan builds `media-index.json` (path, category, size);
re-scan on start (fast, metadata only) and on demand. File hashing is
lazy/optional — never on the critical path.

### No model browser for image/video — with one elegant exception

Held, per design call: no browsing in v1. If it ever earns its place,
the elegant route is the **Hugging Face Hub API**: search by pipeline
tag (`text-to-image`, `image-to-video`, `text-to-video`), list repo
files with sizes, download straight into the correct category folder
under the primary shared root. Model files on HF already live in Comfy
vocabulary. Civitai is *not* elegant (API key, mixed layouts, loose
files) and is explicitly out.

## Terminal & ratatui-image

- Protocol picked at startup with `Picker::from_query_stdio()`: Kitty /
  Sixel / iTerm2 where available, **halfblocks fallback everywhere
  else** (GNOME Terminal/VTE, legacy Windows conhost — both fine,
  just chunkier avatars).
- Avatars + generation previews use `StatefulImage` with
  `ThreadProtocol` so resize/encode never blocks the UI thread.
- Windows note: Windows Terminal ≥ 1.22 speaks Sixel so avatars should
  render there, but ratatui-image doesn't QA it — halfblocks keeps
  Windows usable regardless.
- Everything an image shows is duplicated in text (see Principles).

## Tech choices

- Rust, single crate to start (split into workspace crates only when a
  module earns it). `ratatui` + `crossterm`, `tokio`, `reqwest`
  (rustls), `serde` + `toml` + `serde_json` + `serde_yaml` (Comfy path
  files), `directories` for cross-platform data/config dirs, `image`
  for avatar/result handling.
- **Tested on both of Mel's machines from the start** (Mel, 2026-11-10):
  primary = her Windows laptop (RTX 3060 — that's where Ollama and
  Comfy live), second = her Arch Linux machine. Rust + crossterm is
  cross-platform; OS differences are quarantined in one platform
  module (data dirs, model-path probing, VRAM detection). The repo
  README carries setup steps for both Windows and Arch from day one —
  Windows needs rustup + MSVC build tools, Arch needs rustup +
  base-devel, then `cargo run` is identical on both.

## Roadmap

- **v0 — skeleton that chats.** TUI shell; Agents home; create an agent
  (name, avatar, model, persona files written to disk); Ollama detect +
  installed list; streaming chat; memory/lessons loaded at session start.
- **v1 — models.** Browse / pull with progress / delete with confirm;
  fit badges; `/api/ps` VRAM view.
- **v2 — media library.** Comfy scanner + index; Settings directories;
  Media Library screen.
- **v3 — generation.** Comfy server detect; bundled txt2img templates;
  generate from Media tab; result display; `generate_image` agent tool.
- **v4 — video + yours.** txt2vid template; workflow import + slot
  mapping; websocket live previews.
- **v5 — maybe.** Agent file/shell tools with approvals; HF browser for
  image/video (only if still wanted); multi-agent rooms.

## Open questions

1. Repo: `redibug/ready-bug` on GitHub (electravirus invited, per
   standing rule) — public or private while it's young?
2. Name check: "Ready Bug" is already Bugchan's display name on
   X/Suno — the app sharing it is intentional branding, yes?
3. Does Comfy Desktop live on the same Arch laptop, or another
   machine? (Changes nothing in the design — the server host is a
   setting — but decides what v3 testing looks like.)
