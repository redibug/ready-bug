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
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
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
    /// GPU an install would target on this machine.
    pub gpu: GpuInfo,
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

/// Where the ComfyUI install actually lives. comfy-cli's layout has
/// changed across versions: old releases installed into a `ComfyUI/`
/// subfolder of the workspace, current ones (seen with 1.26) install
/// directly INTO the workspace root. Detect by looking for `main.py`
/// instead of assuming a layout.
pub fn comfyui_dir() -> Option<PathBuf> {
    let ws = workspace();
    let candidates = [
        ws.clone(),
        ws.join("ComfyUI"),
        ws.join("comfy").join("ComfyUI"),
    ];
    for c in candidates {
        if c.join("main.py").is_file() {
            return Some(c);
        }
    }
    // Mid-install fallback: the classic subfolder existing at all is
    // a signal until main.py lands.
    let classic = ws.join("ComfyUI");
    if classic.is_dir() {
        return Some(classic);
    }
    None
}

/// Where a workspace's own (unshared) models would live.
pub fn workspace_models_dir() -> Option<PathBuf> {
    comfyui_dir().map(|d| d.join("models"))
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
        return Some("found".to_string());
    }
    // comfy-cli 1.26 answers --version with a JSON envelope; older
    // versions print a plain version string.
    if text.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            let ver = v
                .get("data")
                .and_then(|d| d.get("version"))
                .or_else(|| v.get("version"))
                .and_then(|s| s.as_str());
            if let Some(ver) = ver {
                return Some(ver.to_string());
            }
        }
    }
    Some(text)
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

/// What kind of GPU `comfy install` should target. Detection order
/// matters on hybrid laptops: a discrete card always beats an
/// integrated one, whatever order the OS lists them in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuKind {
    Nvidia,
    Amd,
    IntelArc,
    AppleSilicon,
    Cpu,
}

impl GpuKind {
    /// The `comfy install` flag for this kind.
    pub fn flag(&self) -> &'static str {
        match self {
            GpuKind::Nvidia => "--nvidia",
            GpuKind::Amd => "--amd",
            GpuKind::IntelArc => "--intel-arc",
            GpuKind::AppleSilicon => "--m-series",
            GpuKind::Cpu => "--cpu",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            GpuKind::Nvidia => "NVIDIA",
            GpuKind::Amd => "AMD",
            GpuKind::IntelArc => "Intel Arc",
            GpuKind::AppleSilicon => "Apple silicon",
            GpuKind::Cpu => "CPU only",
        }
    }
}

#[derive(Clone, Debug)]
pub struct GpuInfo {
    pub kind: GpuKind,
    /// Adapter name when the OS told us one (display only).
    pub name: Option<String>,
}

impl GpuInfo {
    pub fn flag(&self) -> &'static str {
        self.kind.flag()
    }
}

/// Pick a GPU kind from a list of adapter names: NVIDIA first, then
/// AMD/Radeon, then Intel Arc. Integrated Intel/AMD graphics alone
/// don't count — those machines get the CPU build.
pub fn gpu_from_names(names: &[String]) -> Option<(GpuKind, String)> {
    let find = |needles: &[&str]| -> Option<String> {
        names
            .iter()
            .find(|n| {
                let low = n.to_lowercase();
                needles.iter().any(|k| low.contains(k))
            })
            .cloned()
    };
    if let Some(n) = find(&["nvidia", "geforce", "quadro", "tesla"]) {
        return Some((GpuKind::Nvidia, n));
    }
    if let Some(n) = find(&["amd", "radeon"]) {
        // An AMD APU's integrated Radeon is not a ROCm/DirectML target
        // worth picking over CPU unless it looks discrete; the name
        // alone can't tell, so AMD names count (discrete or APU) —
        // comfy's AMD build still beats CPU torch on an APU.
        return Some((GpuKind::Amd, n));
    }
    if let Some(n) = find(&["intel arc", "arc a", "arc b"]) {
        return Some((GpuKind::IntelArc, n));
    }
    None
}

fn run_capture(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Display adapter names from the OS (Windows: CIM via PowerShell;
/// Linux: lspci). Empty when the query isn't available.
fn adapter_names() -> Vec<String> {
    if cfg!(windows) {
        if let Some(text) = run_capture(
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "Get-CimInstance Win32_VideoController | Select-Object -ExpandProperty Name",
            ],
        ) {
            return text
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
        }
        return Vec::new();
    }
    if let Some(text) = run_capture("lspci", &[]) {
        return text
            .lines()
            .filter(|l| {
                let low = l.to_lowercase();
                low.contains("vga") || low.contains("3d controller") || low.contains("display")
            })
            .map(|l| l.trim().to_string())
            .collect();
    }
    Vec::new()
}

/// Detect the GPU `comfy install` should target. nvidia-smi is the
/// ground truth for NVIDIA; Apple silicon is a platform fact;
/// everything else reads the OS adapter list. Falls back to CPU —
/// slow, but it installs anywhere.
pub fn detect_gpu() -> GpuInfo {
    if let Some(text) = run_capture(
        "nvidia-smi",
        &["--query-gpu=name", "--format=csv,noheader"],
    ) {
        let name = text.lines().next().map(|l| l.trim().to_string());
        if let Some(name) = name.filter(|n| !n.is_empty()) {
            return GpuInfo {
                kind: GpuKind::Nvidia,
                name: Some(name),
            };
        }
    }
    if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        return GpuInfo {
            kind: GpuKind::AppleSilicon,
            name: Some("Apple silicon (MPS)".to_string()),
        };
    }
    if let Some((kind, name)) = gpu_from_names(&adapter_names()) {
        return GpuInfo {
            kind,
            name: Some(name),
        };
    }
    GpuInfo {
        kind: GpuKind::Cpu,
        name: None,
    }
}

/// Strip ANSI escape sequences and stray carriage returns from a
/// line of child-process output, and cap its length — pip and
/// comfy both emit progress-bar debris that would wreck the log.
pub fn clean_line(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => {
                // Skip the escape sequence: optional '[' then
                // parameter bytes up to a final letter.
                let mut it = chars.clone();
                if it.next() == Some('[') {
                    let mut consumed = 1;
                    for c2 in it.by_ref() {
                        consumed += 1;
                        if c2.is_ascii_alphabetic() {
                            break;
                        }
                    }
                    for _ in 0..consumed {
                        chars.next();
                    }
                }
            }
            '\r' => {}
            _ => out.push(c),
        }
    }
    let trimmed = out.trim();
    if trimmed.chars().count() > 180 {
        trimmed.chars().take(180).collect()
    } else {
        trimmed.to_string()
    }
}

fn spawn_line_reader<R: std::io::Read + Send + 'static>(
    reader: R,
    sink: Sender<String>,
    acc: Arc<Mutex<Vec<String>>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let buf = BufReader::new(reader);
        for line in buf.lines() {
            let Ok(line) = line else { break };
            let clean = clean_line(&line);
            if clean.is_empty() {
                continue;
            }
            acc.lock().unwrap().push(clean.clone());
            let _ = sink.send(clean);
        }
    })
}

/// Run comfy against OUR workspace (same `--skip-prompt` +
/// `--workspace` discipline as everywhere), streaming each output
/// line to `sink` as it arrives — progress feedback for long
/// operations — and returning the full output at the end.
pub fn run_comfy_streaming(args: &[&str], sink: &Sender<String>) -> Result<String, String> {
    let ws = workspace();
    let mut child = Command::new("comfy")
        .arg("--skip-prompt")
        .arg("--workspace")
        .arg(&ws)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run comfy: {e}"))?;
    let acc = Arc::new(Mutex::new(Vec::new()));
    let t_out = spawn_line_reader(
        child.stdout.take().unwrap(),
        sink.clone(),
        acc.clone(),
    );
    let t_err = spawn_line_reader(
        child.stderr.take().unwrap(),
        sink.clone(),
        acc.clone(),
    );
    let status = child.wait().map_err(|e| format!("comfy failed: {e}"))?;
    let _ = t_out.join();
    let _ = t_err.join();
    let full = acc.lock().unwrap().join("\n");
    if status.success() {
        Ok(full)
    } else {
        Err(if full.trim().is_empty() {
            format!("comfy {} failed", args.join(" "))
        } else {
            full
        })
    }
}

/// Install ComfyUI into our workspace (long-running; call on a
/// thread). The GPU flag is auto-detected — `comfy install` refuses
/// to run unattended without one. Output lines stream to `sink`
/// as they happen so the UI can show live progress.
pub fn install(sink: Sender<String>) -> Result<String, String> {
    let gpu = detect_gpu();
    let picked = match &gpu.name {
        Some(name) => format!("{} — {name}", gpu.kind.label()),
        None => gpu.kind.label().to_string(),
    };
    let _ = sink.send(format!(
        "detected GPU: {picked} → install {}",
        gpu.flag()
    ));
    run_comfy_streaming(&["install", gpu.flag()], &sink)
}

/// Start the daemon in the background (long-running; call on a thread).
pub fn launch(sink: Sender<String>) -> Result<String, String> {
    run_comfy_streaming(&["launch", "--background"], &sink)
}

/// Stop the background daemon.
pub fn stop(sink: Sender<String>) -> Result<String, String> {
    run_comfy_streaming(&["stop"], &sink)
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

pub fn yaml_path() -> Option<PathBuf> {
    comfyui_dir().map(|d| d.join("extra_model_paths.yaml"))
}

/// True when the install's yaml already links the shared library.
pub fn yaml_linked() -> bool {
    let Some(path) = yaml_path() else { return false };
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    text.contains("desktop_shared")
}

/// Write the yaml. Refuses before the workspace is installed —
/// creating a stray ComfyUI folder first would confuse `comfy install`.
pub fn write_yaml() -> Result<PathBuf, String> {
    let Some(_dir) = comfyui_dir() else {
        return Err("workspace not installed yet — install first (i)".to_string());
    };
    let shared = shared_models_dir()
        .ok_or_else(|| "no Comfy Desktop shared library found on this machine".to_string())?;
    let path = yaml_path().expect("comfyui_dir checked above");
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
    if let Some(ws_models) = workspace_models_dir() {
        if ws_models.is_dir() {
            roots.push((ws_models, "ReadyBug workspace".to_string()));
        }
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
        installed: comfyui_dir().is_some(),
        server_up: server_up(),
        server_url: server_url(),
        shared_dir: shared_models_dir(),
        yaml_linked: yaml_linked(),
        gpu: detect_gpu(),
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
    fn clean_line_strips_ansi_and_caps_length() {
        assert_eq!(clean_line("\u{1b}[32mhello\u{1b}[0m world"), "hello world");
        assert_eq!(clean_line("  spaced  \r"), "spaced");
        assert_eq!(clean_line(""), "");
        let long = "x".repeat(300);
        assert_eq!(clean_line(&long).chars().count(), 180);
    }

    #[test]
    fn gpu_priority_discrete_beats_integrated() {
        let hybrid = vec![
            "Intel(R) UHD Graphics".to_string(),
            "NVIDIA GeForce RTX 3060 Laptop GPU".to_string(),
        ];
        let (kind, name) = gpu_from_names(&hybrid).unwrap();
        assert_eq!(kind, GpuKind::Nvidia);
        assert!(name.contains("RTX 3060"));
        assert_eq!(kind.flag(), "--nvidia");

        let amd = vec!["AMD Radeon RX 7800 XT".to_string()];
        assert_eq!(gpu_from_names(&amd).unwrap().0, GpuKind::Amd);

        let arc = vec!["Intel Arc A770".to_string()];
        let (kind, _) = gpu_from_names(&arc).unwrap();
        assert_eq!(kind, GpuKind::IntelArc);
        assert_eq!(kind.flag(), "--intel-arc");

        // Integrated-only machines fall back to CPU.
        let igpu = vec!["Intel(R) Iris(R) Xe Graphics".to_string()];
        assert!(gpu_from_names(&igpu).is_none());
        assert!(gpu_from_names(&[]).is_none());
        assert_eq!(GpuKind::AppleSilicon.flag(), "--m-series");
        assert_eq!(GpuKind::Cpu.flag(), "--cpu");
    }

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

    #[test]
    fn comfyui_dir_detects_both_layouts() {
        let base = std::env::temp_dir().join(format!("rb-layout-test-{}", std::process::id()));
        // Current comfy-cli: ComfyUI installs into the workspace root.
        let root_ws = base.join("rootws");
        fs::create_dir_all(&root_ws).unwrap();
        fs::write(root_ws.join("main.py"), b"# comfy").unwrap();
        // Older comfy-cli: a ComfyUI subfolder inside the workspace.
        let sub_ws = base.join("subws");
        fs::create_dir_all(sub_ws.join("ComfyUI")).unwrap();
        fs::write(sub_ws.join("ComfyUI").join("main.py"), b"# comfy").unwrap();
        // Nothing installed.
        let empty_ws = base.join("emptyws");
        fs::create_dir_all(&empty_ws).unwrap();

        // SAFETY: only this test sets this variable.
        unsafe { std::env::set_var("READYBUG_COMFY_WORKSPACE", &root_ws) };
        assert_eq!(comfyui_dir(), Some(root_ws.clone()));
        unsafe { std::env::set_var("READYBUG_COMFY_WORKSPACE", &sub_ws) };
        assert_eq!(comfyui_dir(), Some(sub_ws.join("ComfyUI")));
        unsafe { std::env::set_var("READYBUG_COMFY_WORKSPACE", &empty_ws) };
        assert_eq!(comfyui_dir(), None);
        unsafe { std::env::remove_var("READYBUG_COMFY_WORKSPACE") };
        let _ = fs::remove_dir_all(&base);
    }
}
