//! ComfyUI via comfy-cli: a headless daemon our apps share.
//!
//! Ready Bug does not reimplement diffusion and does not hijack the
//! user's own comfy-cli setup. It manages ONE dedicated workspace —
//! `~/comfy/ReadyBug` — passed explicitly with `--workspace` on every
//! comfy invocation, so a user's own `comfy` default stays theirs.
//!
//! The daemon is a comfy-cli-managed ComfyUI install run in the
//! background (`comfy launch --background`, :8188). Models are NOT
//! stored in the workspace: an `extra_model_paths.yaml` inside the
//! install points ComfyUI at the Comfy Desktop shared library, so
//! Desktop, the daemon, and every app scanning that library all see
//! the same files. Duplicates across scanned roots are flagged for
//! the user — never auto-deleted (the user's files, the user's call).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Model file extensions the scanner recognises (ComfyUI's set).
const MODEL_EXTS: [&str; 7] = ["safetensors", "gguf", "ckpt", "pt", "pth", "bin", "sft"];

/// ComfyUI's category folder names (the taxonomy the scanner files
/// models under; aliases included so A1111/Matrix-style layouts sort
/// sensibly when a user adds one as an extra root).
const CATEGORIES: [&str; 16] = [
    "checkpoints",
    "diffusion_models",
    "unet",
    "text_encoders",
    "clip",
    "clip_vision",
    "vae",
    "vae_approx",
    "loras",
    "controlnet",
    "upscale_models",
    "embeddings",
    "ipadapter",
    "style_models",
    "gligen",
    "hypernetworks",
];

#[derive(Clone, Debug)]
pub struct MediaModel {
    pub name: String,
    pub category: String,
    pub size: u64,
    /// Short label of the root this file was found under.
    pub source: String,
    pub path: PathBuf,
    /// Same file name + size exists at another scanned path too.
    /// A warning for the user — dedupe is their responsibility.
    pub duplicate: bool,
}

#[derive(Clone, Debug)]
pub struct ComfyStatus {
    /// comfy-cli version string, None = the CLI was not found.
    pub cli_version: Option<String>,
    pub workspace: PathBuf,
    /// The workspace holds a ComfyUI install.
    pub installed: bool,
    /// Something answered on the ComfyUI port.
    pub server_up: bool,
    pub server_url: String,
    /// Comfy Desktop shared library, if present on this machine.
    pub shared_dir: Option<PathBuf>,
    /// extra_model_paths.yaml already points at the shared library.
    pub yaml_linked: bool,
    pub models: Vec<MediaModel>,
    pub duplicate_count: usize,
}

/// The dedicated workspace our apps share. Override with
/// READYBUG_COMFY_WORKSPACE (tests, unusual setups).
pub fn workspace() -> PathBuf {
    if let Ok(d) = std::env::var("READYBUG_COMFY_WORKSPACE") {
        if !d.trim().is_empty() {
            return PathBuf::from(d);
        }
    }
    if let Some(home) = directories::BaseDirs::new() {
        return home.home_dir().join("comfy").join("ReadyBug");
    }
    PathBuf::from("comfy").join("ReadyBug")
}

/// The ComfyUI install inside the workspace.
pub fn comfyui_dir() -> PathBuf {
    workspace().join("ComfyUI")
}

/// Where a workspace's own (unshared) models would live.
pub fn workspace_models_dir() -> PathBuf {
    comfyui_dir().join("models")
}

/// The ComfyUI server the daemon serves. COMFY_HOST overrides, in the
/// same spirit as OLLAMA_HOST.
pub fn server_url() -> String {
    let raw = std::env::var("COMFY_HOST").unwrap_or_else(|_| "http://localhost:8188".to_string());
    let raw = raw.trim().trim_end_matches('/').to_string();
    if raw.starts_with("http://") || raw.starts_with("https://") {
        raw
    } else {
        format!("http://{raw}")
    }
}

/// Comfy Desktop's shared model library on this machine, if present.
/// Override with READYBUG_COMFY_SHARED.
pub fn shared_models_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("READYBUG_COMFY_SHARED") {
        if !d.trim().is_empty() {
            let p = PathBuf::from(d);
            return p.is_dir().then_some(p);
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            candidates.push(
                PathBuf::from(local)
                    .join("Comfy-Desktop")
                    .join("ComfyUI-Shared")
                    .join("models"),
            );
        }
    }
    if let Some(home) = directories::BaseDirs::new() {
        candidates.push(home.home_dir().join("ComfyUI-Shared").join("models"));
    }
    candidates.into_iter().find(|p| p.is_dir())
}

/// Extra roots the user asked us to scan, from READYBUG_MODEL_DIRS
/// (OS path-list separated). Settings UI for this comes later; the
/// env var is the v1 mechanism.
pub fn extra_scan_dirs() -> Vec<PathBuf> {
    let Ok(d) = std::env::var("READYBUG_MODEL_DIRS") else {
        return Vec::new();
    };
    std::env::split_paths(&d).filter(|p| p.is_dir()).collect()
}

fn cli_version() -> Option<String> {
    let out = Command::new("comfy")
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        Some("found".to_string())
    } else {
        Some(text)
    }
}

fn server_up() -> bool {
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(900))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    client
        .get(format!("{}/system_stats", server_url()))
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Run comfy against OUR workspace. Every invocation carries
/// `--workspace` — the user's own comfy default is never touched.
fn run_comfy(args: &[&str]) -> Result<String, String> {
    let ws = workspace();
    let out = Command::new("comfy")
        .arg("--workspace")
        .arg(&ws)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run comfy: {e}"))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&err);
    }
    if out.status.success() {
        Ok(text.trim().to_string())
    } else {
        Err(if text.trim().is_empty() {
            format!("comfy {} failed", args.join(" "))
        } else {
            text.trim().to_string()
        })
    }
}

/// Install ComfyUI into our workspace (long-running; call on a thread).
pub fn install() -> Result<String, String> {
    run_comfy(&["install"])
}

/// Start the daemon in the background (long-running; call on a thread).
pub fn launch() -> Result<String, String> {
    run_comfy(&["launch", "--background"])
}

/// Stop the background daemon.
pub fn stop() -> Result<String, String> {
    run_comfy(&["stop"])
}

/// The extra_model_paths.yaml body linking the install to the Comfy
/// Desktop shared library. Forward slashes everywhere: Windows
/// accepts them and YAML escaping stays painless.
pub fn yaml_content(shared: &Path) -> String {
    let base = shared.to_string_lossy().replace('\\', "/");
    let mut s = String::from(
        "# Written by Ready Bug: share Comfy Desktop's model library\n\
         # with this install instead of keeping a second copy.\n\
         desktop_shared:\n",
    );
    s.push_str(&format!("    base_path: {base}/\n"));
    for key in [
        "checkpoints",
        "diffusion_models",
        "text_encoders",
        "clip_vision",
        "vae",
        "loras",
        "controlnet",
        "upscale_models",
        "embeddings",
    ] {
        s.push_str(&format!("    {key}: {key}\n"));
    }
    s
}

pub fn yaml_path() -> PathBuf {
    comfyui_dir().join("extra_model_paths.yaml")
}

/// True when the install's yaml already links the shared library.
pub fn yaml_linked() -> bool {
    let Ok(text) = fs::read_to_string(yaml_path()) else {
        return false;
    };
    text.contains("desktop_shared")
}

/// Write the yaml. Refuses before the workspace is installed —
/// creating a stray ComfyUI folder first would confuse `comfy install`.
pub fn write_yaml() -> Result<PathBuf, String> {
    let dir = comfyui_dir();
    if !dir.is_dir() {
        return Err("workspace not installed yet — install first (i)".to_string());
    }
    let shared = shared_models_dir()
        .ok_or_else(|| "no Comfy Desktop shared library found on this machine".to_string())?;
    let path = yaml_path();
    fs::write(&path, yaml_content(&shared)).map_err(|e| format!("could not write yaml: {e}"))?;
    Ok(path)
}

fn is_model_file(p: &Path) -> bool {
    p.extension()
        .map(|e| MODEL_EXTS.contains(&e.to_string_lossy().to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn walk_models(scan_root: &Path, dir: &Path, depth: usize, out: &mut Vec<(PathBuf, String)>) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            walk_models(scan_root, &path, depth + 1, out);
        } else if is_model_file(&path) {
            // Category = the ancestor folder just under the scanned
            // root when it is one of ComfyUI's category names.
            let rel = path.strip_prefix(scan_root).unwrap_or(&path);
            let first = rel
                .components()
                .next()
                .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase());
            let category = match first {
                Some(f) if CATEGORIES.contains(&f.as_str()) => f,
                _ => "other".to_string(),
            };
            out.push((path, category));
        }
    }
}

/// Scan every known root and merge the results. Foreign roots are
/// read, never written. Possible duplicates (same name + size at
/// different paths) are flagged, not resolved.
pub fn scan_models() -> Vec<MediaModel> {
    let mut roots: Vec<(PathBuf, String)> = Vec::new();
    if let Some(shared) = shared_models_dir() {
        roots.push((shared, "Comfy Desktop shared".to_string()));
    }
    let ws_models = workspace_models_dir();
    if ws_models.is_dir() {
        roots.push((ws_models, "ReadyBug workspace".to_string()));
    }
    for extra in extra_scan_dirs() {
        let label = format!("extra: {}", extra.display());
        roots.push((extra, label));
    }

    let mut models: Vec<MediaModel> = Vec::new();
    for (root, source) in &roots {
        let mut found = Vec::new();
        walk_models(root, root, 0, &mut found);
        for (path, category) in found {
            let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            models.push(MediaModel {
                name,
                category,
                size,
                source: source.clone(),
                path,
                duplicate: false,
            });
        }
    }
    models.sort_by(|a, b| {
        a.category
            .cmp(&b.category)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    // Flag possible duplicates: same file name (case-insensitive) and
    // same size, at two or more distinct paths. Same path reached via
    // two roots is one file, not a duplicate.
    let mut by_key: HashMap<(String, u64), Vec<usize>> = HashMap::new();
    for (i, m) in models.iter().enumerate() {
        by_key
            .entry((m.name.to_lowercase(), m.size))
            .or_default()
            .push(i);
    }
    for idxs in by_key.values() {
        if idxs.len() < 2 {
            continue;
        }
        let distinct: std::collections::HashSet<&PathBuf> =
            idxs.iter().map(|&i| &models[i].path).collect();
        if distinct.len() < 2 {
            continue;
        }
        for &i in idxs {
            models[i].duplicate = true;
        }
    }
    models
}

/// Gather the full picture: CLI, workspace, daemon, library, models.
pub fn gather() -> ComfyStatus {
    let models = scan_models();
    let duplicate_count = models.iter().filter(|m| m.duplicate).count();
    ComfyStatus {
        cli_version: cli_version(),
        workspace: workspace(),
        installed: comfyui_dir().is_dir(),
        server_up: server_up(),
        server_url: server_url(),
        shared_dir: shared_models_dir(),
        yaml_linked: yaml_linked(),
        models,
        duplicate_count,
    }
}

/// Human size, same house style as the Ollama model list.
pub fn format_size(bytes: u64) -> String {
    crate::ollama::format_size(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_points_at_shared_with_categories() {
        let y = yaml_content(Path::new("C:/Users/mel/AppData/Local/Comfy-Desktop/ComfyUI-Shared/models"));
        assert!(y.contains("desktop_shared:"));
        assert!(y.contains(
            "base_path: C:/Users/mel/AppData/Local/Comfy-Desktop/ComfyUI-Shared/models/"
        ));
        assert!(y.contains("checkpoints: checkpoints"));
        assert!(y.contains("loras: loras"));
    }

    #[test]
    fn scan_finds_models_and_flags_duplicates() {
        let base = std::env::temp_dir().join(format!("rb-comfy-test-{}", std::process::id()));
        let a = base.join("a").join("checkpoints");
        let b = base.join("b").join("loras");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("flux.safetensors"), vec![0u8; 128]).unwrap();
        fs::write(b.join("flux.safetensors"), vec![0u8; 128]).unwrap();
        fs::write(b.join("style.safetensors"), vec![0u8; 64]).unwrap();
        fs::write(b.join("notes.txt"), b"not a model").unwrap();

        // SAFETY: this test is the only one touching these variables,
        // and it restores them before asserting.
        unsafe {
            std::env::set_var(
                "READYBUG_MODEL_DIRS",
                std::env::join_paths([base.join("a"), base.join("b")]).unwrap(),
            );
            std::env::set_var("READYBUG_COMFY_SHARED", base.join("missing"));
        }
        let models = scan_models();
        unsafe {
            std::env::remove_var("READYBUG_MODEL_DIRS");
            std::env::remove_var("READYBUG_COMFY_SHARED");
        }

        assert_eq!(models.len(), 3);
        let dups: Vec<&MediaModel> = models.iter().filter(|m| m.duplicate).collect();
        assert_eq!(dups.len(), 2);
        assert!(models.iter().all(|m| m.name != "notes.txt"));
        let ckpt = models.iter().find(|m| m.name == "flux.safetensors").unwrap();
        assert_eq!(ckpt.category, "checkpoints");
        let _ = fs::remove_dir_all(&base);
    }
}
