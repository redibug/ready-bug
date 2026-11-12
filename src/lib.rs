//! Ready Bug core library — everything that isn't the TUI.
//!
//! Other programs (the text adventure games, future apps) depend on
//! this crate for the shared local-AI plumbing: where the ComfyUI
//! workspace lives, whether the daemon is up, how Ollama answers —
//! and drive setup through the `readybug` CLI binary, which is a
//! thin shell over these same modules. One implementation, many
//! front doors (Mel's platform call, 2026-11-12).

pub mod comfy;
pub mod ollama;
pub mod setup;
