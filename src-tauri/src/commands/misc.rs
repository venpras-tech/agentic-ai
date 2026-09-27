//! Miscellaneous lightweight IPC: console log history, auto-update probes, the
//! MCP server catalog, and the headless boot-smoke probes. Moved out of
//! `main.rs` during the command-layer split.

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::agent::mcp::McpServerConfig;
use crate::agent::ToolState;
use crate::{app_config_dir, smoke_enabled};

/// System hardware snapshot used by the model-fit estimator.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuInfo {
    pub name: String,
    pub vram_gb: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo {
    pub os: String,
    pub cpu: String,
    pub cores: usize,
    pub total_ram_gb: f64,
    pub available_ram_gb: f64,
    pub total_swap_gb: f64,
    pub disk_free_gb: f64,
    pub disk_total_gb: f64,
    pub gpu: Option<GpuInfo>,
    /// True when the binary was compiled with a llama.cpp GPU backend.
    pub gpu_enabled: bool,
}

fn parse_gb_nvidia(lines: &str) -> Option<GpuInfo> {
    // nvidia-smi CSV output: "NVIDIA GeForce RTX 4070, 12 GiB"
    for line in lines.lines() {
        let mut parts = line.split(',');
        let name = parts.next().unwrap_or_default().trim().to_string();
        let vram = parts
            .next()
            .map(|s| s.trim().split_whitespace().next().unwrap_or("0").replace(',', ""))
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        if !name.is_empty() {
            return Some(GpuInfo { name, vram_gb: vram });
        }
    }
    None
}

fn query_nvidia_gpu() -> Option<GpuInfo> {
    let mut child = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let out = child.stdout.take()?;
    use std::io::Read;
    let mut buf = String::new();
    let _ = std::io::BufReader::new(out).read_to_string(&mut buf);
    let _ = child.wait();
    parse_gb_nvidia(&buf)
}

/// Collect host hardware (RAM, CPU, disks, GPU) for the Models page's
/// "will this model fit?" estimator.
#[tauri::command]
pub async fn system_info(app: AppHandle) -> Result<SystemInfo, String> {
    use sysinfo::{Disks, System};

    let mut sys = System::new_all();
    sys.refresh_all();

    let os = std::env::consts::OS.to_string();
    let cpu = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Unknown CPU".into());
    let cores = sys.physical_core_count().unwrap_or_else(|| sys.cpus().len());

    let disk = {
        let dir = crate::app_data_dir(&app);
        let disks = Disks::new_with_refreshed_list();
        let by_prefix = disks
            .list()
            .iter()
            .find(|d| dir.starts_with(d.mount_point()))
            .map(|d| (d.available_space(), d.total_space()));
        by_prefix
            .or_else(|| disks.list().first().map(|d| (d.available_space(), d.total_space())))
            .unwrap_or((0, 0))
    };

    let gpu = query_nvidia_gpu();
    let gpu_enabled = cfg!(feature = "gpu-cuda");

    Ok(SystemInfo {
        os,
        cpu,
        cores,
        total_ram_gb: sys.total_memory() as f64 / (1024.0 * 1024.0 * 1024.0),
        available_ram_gb: sys.available_memory() as f64 / (1024.0 * 1024.0 * 1024.0),
        total_swap_gb: sys.total_swap() as f64 / (1024.0 * 1024.0 * 1024.0),
        disk_free_gb: disk.0 as f64 / (1024.0 * 1024.0 * 1024.0),
        disk_total_gb: disk.1 as f64 / (1024.0 * 1024.0 * 1024.0),
        gpu,
        gpu_enabled,
    })
}

/// Replay the recent log lines so the webview Console window shows history
/// captured before its listeners attached (startup, model auto-load, …).
#[tauri::command]
pub async fn console_history() -> Result<Vec<String>, String> {
    Ok(crate::logging::recent_lines())
}

// ---- auto-update (opt-in via AI_EDITOR_UPDATER_PUBKEY) ----

/// True when this build ships a signing key, i.e. has a live release channel.
/// The frontend uses it to decide whether any update UI makes sense.
#[tauri::command]
pub async fn updater_configured() -> bool {
    std::env::var("AI_EDITOR_UPDATER_PUBKEY")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

fn updater_endpoints() -> Result<Vec<url::Url>, String> {
    let raw = std::env::var("AI_EDITOR_UPDATER_ENDPOINTS").unwrap_or_else(|_| {
        "https://releases.ai-editor.dev/{{target}}/{{arch}}/{{current_version}}".to_string()
    });
    raw.split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<url::Url>()
                .map_err(|e| format!("bad updater endpoint `{s}`: {e}"))
        })
        .collect()
}

/// Probe the release feed. Returns the newest version string when an update
/// exists, None when current — or when updates aren't configured at all.
#[tauri::command]
pub async fn update_check(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_updater::UpdaterExt;
    if !updater_configured().await {
        return Ok(None);
    }
    let updater = app
        .updater_builder()
        .endpoints(updater_endpoints()?)
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())?;
    Ok(updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .map(|u| u.version))
}

/// Load the user's persisted MCP server catalog for the settings UI.
#[tauri::command]
pub async fn mcp_catalog_load(app: AppHandle) -> Result<Vec<McpServerConfig>, String> {
    crate::agent::mcp::load_catalog(&app_config_dir(&app))
}

/// Replace the user's MCP server catalog from the settings UI.
#[tauri::command]
pub async fn mcp_catalog_save(
    app: AppHandle,
    state: State<'_, Arc<ToolState>>,
    servers: Vec<McpServerConfig>,
) -> Result<(), String> {
    // Validate before writing: unique non-empty names, non-empty commands.
    let mut seen = std::collections::HashSet::new();
    for s in &servers {
        if s.name.trim().is_empty() {
            return Err("Server name must not be empty".into());
        }
        if !seen.insert(s.name.clone()) {
            return Err(format!("Duplicate server name `{}`", s.name));
        }
        if s.bin.trim().is_empty() {
            return Err(format!("Server `{}` needs a command", s.name));
        }
    }
    // Evict cached connections that no longer match a live config entry.
    let stale: Vec<String> = {
        let cache = state.mcp_servers.lock().await;
        cache
            .keys()
            .filter(|key| {
                !servers
                    .iter()
                    .any(|s| s.enabled && **key == format!("{} {}", s.bin, s.args.join(" ")))
            })
            .cloned()
            .collect()
    };
    {
        let mut cache = state.mcp_servers.lock().await;
        for key in stale {
            cache.remove(&key);
        }
    }
    crate::agent::mcp::save_catalog(&app_config_dir(&app), &servers)
}

/// Probe used by the frontend to decide whether to report boot success.
#[tauri::command]
pub async fn smoke_active() -> bool {
    smoke_enabled()
}

/// Frontend ack: the webview mounted and React rendered. Exits green.
#[tauri::command]
pub async fn smoke_boot_ok(app: AppHandle) -> Result<(), String> {
    if smoke_enabled() {
        println!("AI_EDITOR_SMOKE_OK");
        app.exit(0);
    }
    Ok(())
}

/// Frontend boot-failure report: surfaces webview-side errors (module
/// evaluation crashes, render errors, failed invokes) on stderr so a headless
/// smoke run shows *why* boot never completed instead of just timing out.
#[tauri::command]
pub async fn smoke_fail(message: String) -> Result<(), String> {
    eprintln!("AI_EDITOR_SMOKE_FAIL: {message}");
    Ok(())
}