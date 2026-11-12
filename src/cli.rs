//! `readybug` — the Ready Bug CLI.
//!
//! The setup/control surface other programs use instead of carrying
//! their own copy of this logic (Mel's platform call, 2026-11-12):
//! a game checks for this binary, downloads it from GitHub Releases
//! if missing, and runs `readybug install`. Everything here is a
//! thin shell over the `ready_bug` library — the TUI uses the same
//! modules, so the two can never drift apart.

use ready_bug::{comfy, setup};
use std::io::Write;
use std::sync::mpsc::{channel, Sender};

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn usage() {
    println!(
        "readybug {VERSION} — shared local-AI setup & ComfyUI control

USAGE:
    readybug                launch the app (in a terminal)
    readybug tui            same, explicitly
    readybug install            install prerequisites (Ollama, Python 3.13,
                                comfy-cli), then install ComfyUI into the
                                shared ReadyBug workspace and link the
                                Comfy Desktop shared model library
    readybug doctor [--json]    report what's present / missing
    readybug comfy status [--json]
    readybug comfy launch       start the ComfyUI daemon (background)
    readybug comfy stop         stop the ComfyUI daemon
    readybug --version

Exit codes: 0 ok, 1 something failed, 2 bad usage."
    );
}

/// Run a library action that streams lines through an mpsc sink,
/// printing them as they arrive.
fn run_streamed(
    f: impl FnOnce(Sender<String>) -> Result<String, String>,
) -> Result<String, String> {
    let (tx, rx) = channel::<String>();
    let printer = std::thread::spawn(move || {
        let mut out = std::io::stdout().lock();
        for line in rx {
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    });
    let result = f(tx);
    let _ = printer.join();
    result
}

fn doctor(json: bool) -> i32 {
    let prereqs = setup::check_all();
    let st = comfy::gather();
    if json {
        let doc = serde_json::json!({
            "readybug": VERSION,
            "prerequisites": prereqs.iter().map(|p| serde_json::json!({
                "name": p.name, "ok": p.ok, "detail": p.detail
            })).collect::<Vec<_>>(),
            "workspace": st.workspace.display().to_string(),
            "comfy_cli": st.cli_version,
            "installed": st.installed,
            "daemon_up": st.server_up,
            "daemon_url": st.server_url,
            "shared_library": st.shared_dir.as_ref().map(|d| d.display().to_string()),
            "yaml_linked": st.yaml_linked,
            "gpu": serde_json::json!({
                "name": st.gpu.name, "flag": st.gpu.flag()
            }),
            "models": st.models.len(),
            "possible_duplicates": st.duplicate_count,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&doc).unwrap_or_default()
        );
        return 0;
    }
    println!("readybug {VERSION} — doctor");
    for p in &prereqs {
        println!("  {} {} — {}", if p.ok { "✓" } else { "✗" }, p.name, p.detail);
    }
    println!(
        "  {} comfy workspace — {} ({})",
        if st.installed { "✓" } else { "✗" },
        st.workspace.display(),
        if st.installed { "installed" } else { "not installed" }
    );
    println!(
        "  {} daemon — {}",
        if st.server_up { "●" } else { "○" },
        if st.server_up {
            format!("up at {}", st.server_url)
        } else {
            "not running".to_string()
        }
    );
    match &st.shared_dir {
        Some(d) => println!(
            "  ✓ shared library — {} ({})",
            d.display(),
            if st.yaml_linked { "linked" } else { "not linked" }
        ),
        None => println!("  ○ shared library — no Comfy Desktop library found"),
    }
    println!(
        "  models: {} scanned, {} possible duplicate(s)",
        st.models.len(),
        st.duplicate_count
    );
    0
}

fn comfy_status(json: bool) -> i32 {
    let st = comfy::gather();
    if json {
        let doc = serde_json::json!({
            "workspace": st.workspace.display().to_string(),
            "comfy_cli": st.cli_version,
            "installed": st.installed,
            "daemon_up": st.server_up,
            "daemon_url": st.server_url,
            "shared_library": st.shared_dir.as_ref().map(|d| d.display().to_string()),
            "yaml_linked": st.yaml_linked,
            "models": st.models.len(),
            "possible_duplicates": st.duplicate_count,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&doc).unwrap_or_default()
        );
        return 0;
    }
    println!("workspace:  {}", st.workspace.display());
    println!(
        "comfy-cli:  {}",
        st.cli_version.as_deref().unwrap_or("not found")
    );
    println!(
        "installed:  {}",
        if st.installed { "yes" } else { "no — run `readybug install`" }
    );
    println!(
        "daemon:     {}",
        if st.server_up {
            format!("up at {}", st.server_url)
        } else {
            "not running".to_string()
        }
    );
    println!(
        "models:     {} scanned, {} possible duplicate(s)",
        st.models.len(),
        st.duplicate_count
    );
    0
}

fn install() -> i32 {
    println!("readybug {VERSION} — install");
    println!("[1/3] prerequisites");
    let prereq_result = run_streamed(|tx| {
        setup::ensure_prereqs(&tx).map(|_| "prerequisites ok".to_string())
    });
    if let Err(e) = prereq_result {
        eprintln!("install stopped: {e}");
        eprintln!("fix the above, then run `readybug install` again — it's safe to re-run");
        return 1;
    }

    println!("[2/3] ComfyUI (workspace {})", comfy::workspace().display());
    if comfy::gather().installed {
        println!("already installed — skipping (re-running is the TUI's install key)");
    } else {
        match run_streamed(comfy::install) {
            Ok(_) => println!("✓ ComfyUI installed"),
            Err(e) => {
                eprintln!("ComfyUI install failed: {e}");
                return 1;
            }
        }
    }

    println!("[3/3] shared model library");
    match comfy::shared_models_dir() {
        Some(shared) => {
            if comfy::yaml_linked() {
                println!("already linked — {}", shared.display());
            } else {
                match comfy::write_yaml() {
                    Ok(path) => println!("✓ linked {} via {}", shared.display(), path.display()),
                    Err(e) => {
                        eprintln!("could not link the shared library: {e}");
                        return 1;
                    }
                }
            }
        }
        None => println!("no Comfy Desktop shared library on this machine — skipped"),
    }

    println!("done. Start the daemon with: readybug comfy launch");
    0
}

pub fn run(args: &[String]) -> i32 {
    let strs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    match strs.as_slice() {
        [] => {
            usage();
            0
        }
        ["--help"] | ["-h"] | ["help"] => {
            usage();
            0
        }
        ["--version"] | ["-V"] | ["version"] => {
            println!("readybug {VERSION}");
            0
        }
        ["doctor"] => doctor(false),
        ["doctor", "--json"] => doctor(true),
        ["install"] => install(),
        ["comfy", "status"] => comfy_status(false),
        ["comfy", "status", "--json"] => comfy_status(true),
        ["comfy", "launch"] => match run_streamed(comfy::launch) {
            Ok(_) => {
                println!("daemon starting — it answers on {} once up", comfy::server_url());
                0
            }
            Err(e) => {
                eprintln!("launch failed: {e}");
                1
            }
        },
        ["comfy", "stop"] => match run_streamed(comfy::stop) {
            Ok(_) => {
                println!("daemon stopped");
                0
            }
            Err(e) => {
                eprintln!("stop failed: {e}");
                1
            }
        },
        _ => {
            eprintln!("unknown command: {}", args.join(" "));
            usage();
            2
        }
    }
}
