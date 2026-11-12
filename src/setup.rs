//! Prerequisite bootstrap for `readybug install`.
//!
//! The three things the ComfyUI stack needs before Ready Bug's own
//! install can run: **Ollama**, **Python 3.13**, and **comfy-cli**.
//! Each has a check (is the requirement satisfied?) and an install
//! (per-user routes only — no admin prompts). Every install
//! verifies itself by re-running the check. Progress streams
//! through an mpsc sink, the same pattern as comfy's runner.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;

pub struct Prereq {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

fn capture(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Run a command, forwarding stdout+stderr lines to the sink as
/// they arrive. Returns whether it exited successfully.
fn stream(cmd: &str, args: &[&str], sink: &Sender<String>) -> bool {
    let mut child = match Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = sink.send(format!("could not start {cmd}: {e}"));
            return false;
        }
    };
    let mut handles = Vec::new();
    if let Some(out) = child.stdout.take() {
        let tx = sink.clone();
        handles.push(std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                let line = crate::comfy::clean_line(&line);
                if !line.is_empty() {
                    let _ = tx.send(line);
                }
            }
        }));
    }
    if let Some(err) = child.stderr.take() {
        let tx = sink.clone();
        handles.push(std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                let line = crate::comfy::clean_line(&line);
                if !line.is_empty() {
                    let _ = tx.send(line);
                }
            }
        }));
    }
    let ok = child.wait().map(|s| s.success()).unwrap_or(false);
    for h in handles {
        let _ = h.join();
    }
    ok
}

fn winget_available() -> bool {
    capture("winget", &["--version"]).is_some()
}

fn winget_install(id: &str, sink: &Sender<String>) -> bool {
    stream(
        "winget",
        &[
            "install",
            "--id",
            id,
            "-e",
            "--accept-source-agreements",
            "--accept-package-agreements",
        ],
        sink,
    )
}

// ------------------------------------------------------------------
// Checks
// ------------------------------------------------------------------

pub fn check_ollama() -> Prereq {
    match capture("ollama", &["--version"]) {
        Some(v) => Prereq {
            name: "ollama",
            ok: true,
            detail: v,
        },
        None => Prereq {
            name: "ollama",
            ok: false,
            detail: "not found".to_string(),
        },
    }
}

fn parse_python_version(text: &str) -> Option<(u32, u32)> {
    // "Python 3.13.0" → (3, 13)
    let num = text.trim().strip_prefix("Python ")?.trim();
    let mut parts = num.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// The python command prefix to use, preferring an explicit 3.13.
/// Returns (prefix, (major, minor)) for the best candidate found.
pub fn find_python() -> Option<(Vec<String>, (u32, u32))> {
    let candidates: Vec<Vec<String>> = if cfg!(windows) {
        vec![
            vec!["py".into(), "-3.13".into()],
            vec!["py".into()],
            vec!["python".into()],
        ]
    } else {
        vec![
            vec!["python3.13".into()],
            vec!["python3".into()],
            vec!["python".into()],
        ]
    };
    let mut best: Option<(Vec<String>, (u32, u32))> = None;
    for cand in candidates {
        let args: Vec<&str> = cand[1..].iter().map(|s| s.as_str()).collect();
        let mut full = args.clone();
        full.push("--version");
        if let Some(text) = capture(&cand[0], &full) {
            if let Some(ver) = parse_python_version(&text) {
                if ver >= (3, 13) {
                    return Some((cand, ver));
                }
                if best.as_ref().map(|(_, b)| ver > *b).unwrap_or(true) {
                    best = Some((cand, ver));
                }
            }
        }
    }
    best
}

pub fn check_python() -> Prereq {
    match find_python() {
        Some((_, (major, minor))) if (major, minor) >= (3, 13) => Prereq {
            name: "python",
            ok: true,
            detail: format!("Python {major}.{minor}"),
        },
        Some((_, (major, minor))) => Prereq {
            name: "python",
            ok: false,
            detail: format!(
                "Python {major}.{minor} found, but ComfyUI wants 3.13"
            ),
        },
        None => Prereq {
            name: "python",
            ok: false,
            detail: "not found".to_string(),
        },
    }
}

pub fn check_comfy_cli() -> Prereq {
    match crate::comfy::cli_version() {
        Some(v) => Prereq {
            name: "comfy-cli",
            ok: true,
            detail: format!("comfy-cli {v}"),
        },
        None => Prereq {
            name: "comfy-cli",
            ok: false,
            detail: "not found".to_string(),
        },
    }
}

pub fn check_all() -> Vec<Prereq> {
    vec![check_ollama(), check_python(), check_comfy_cli()]
}

// ------------------------------------------------------------------
// Installs (per-user routes; each verifies via its check afterwards)
// ------------------------------------------------------------------

pub fn install_ollama(sink: &Sender<String>) -> Result<(), String> {
    let attempted = if cfg!(windows) {
        if winget_available() {
            winget_install("Ollama.Ollama", sink)
        } else {
            let _ = sink.send(
                "winget not available — install Ollama from https://ollama.com/download"
                    .to_string(),
            );
            false
        }
    } else if cfg!(target_os = "macos") {
        if capture("brew", &["--version"]).is_some() {
            stream("brew", &["install", "ollama"], sink)
        } else {
            let _ = sink
                .send("install Ollama from https://ollama.com/download".to_string());
            false
        }
    } else {
        // Official install script (may ask for sudo in the terminal).
        stream(
            "sh",
            &["-c", "curl -fsSL https://ollama.com/install.sh | sh"],
            sink,
        )
    };
    if attempted && check_ollama().ok {
        Ok(())
    } else {
        Err("ollama still not available after install attempt".to_string())
    }
}

pub fn install_python(sink: &Sender<String>) -> Result<(), String> {
    let attempted = if cfg!(windows) {
        if winget_available() {
            winget_install("Python.Python.3.13", sink)
        } else {
            let _ = sink.send(
                "winget not available — install Python 3.13 from https://www.python.org/downloads/ (tick 'Add python.exe to PATH')"
                    .to_string(),
            );
            false
        }
    } else if cfg!(target_os = "macos") {
        if capture("brew", &["--version"]).is_some() {
            stream("brew", &["install", "python@3.13"], sink)
        } else {
            let _ = sink.send(
                "install Python 3.13 from https://www.python.org/downloads/".to_string(),
            );
            false
        }
    } else {
        let _ = sink.send(
            "no universal per-user Python install on Linux — use your distro (e.g. `sudo apt install python3.13`, `sudo pacman -S python`) and re-run"
                .to_string(),
        );
        false
    };
    if attempted && check_python().ok {
        Ok(())
    } else {
        Err("python 3.13 still not available after install attempt".to_string())
    }
}

pub fn install_comfy_cli(sink: &Sender<String>) -> Result<(), String> {
    let (prefix, _) = find_python()
        .ok_or_else(|| "python is required before comfy-cli can be installed".to_string())?;
    let mut args: Vec<&str> = prefix[1..].iter().map(|s| s.as_str()).collect();
    args.extend(["-m", "pip", "install"]);
    if !cfg!(windows) {
        args.push("--user");
    }
    args.push("comfy-cli");
    let attempted = stream(&prefix[0], &args, sink);
    if attempted && check_comfy_cli().ok {
        Ok(())
    } else {
        Err("comfy-cli still not available after install attempt".to_string())
    }
}

/// Check everything; install what's missing; verify. Err names the
/// prerequisites that could not be satisfied.
pub fn ensure_prereqs(sink: &Sender<String>) -> Result<Vec<Prereq>, String> {
    let mut failures: Vec<String> = Vec::new();
    for check in check_all() {
        if check.ok {
            let _ = sink.send(format!("✓ {} ({})", check.name, check.detail));
            continue;
        }
        let _ = sink.send(format!(
            "… {}: {} — installing",
            check.name, check.detail
        ));
        let result = match check.name {
            "ollama" => install_ollama(sink),
            "python" => install_python(sink),
            _ => install_comfy_cli(sink),
        };
        match result {
            Ok(()) => {
                let _ = sink.send(format!("✓ {} installed", check.name));
            }
            Err(e) => {
                let _ = sink.send(format!("✗ {}: {e}", check.name));
                failures.push(check.name.to_string());
            }
        }
    }
    if failures.is_empty() {
        Ok(check_all())
    } else {
        Err(format!(
            "could not install: {}",
            failures.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_version_parsing() {
        assert_eq!(parse_python_version("Python 3.13.0"), Some((3, 13)));
        assert_eq!(parse_python_version("Python 3.9.7"), Some((3, 9)));
        assert_eq!(parse_python_version("not python"), None);
    }
}
