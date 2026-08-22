//! Build-time accelerator autodetection.
//!
//! Inspects the host toolchain and emits cfg flags the runtime can consult,
//! plus actionable guidance when a GPU backend is present but not enabled:
//!
//! * **macOS** → Apple Metal is the system GPU API on every supported machine;
//!   emits `ai_accel_metal`. The llama.cpp Metal backend is wired automatically
//!   in `Cargo.toml` via the platform-specific dependency entry.
//! * **Windows / Linux** → scans for `nvcc` (PATH + well-known CUDA install
//!   dirs); emits `ai_accel_cuda` and validates the pairing with the
//!   `gpu-cuda` Cargo feature.
//!
//! # Why features and not flags here
//!
//! Cargo resolves dependency features *before* build scripts run, so a build
//! script cannot switch on a dependency feature by itself. The actual llama.cpp
//! backend toggle lives in `Cargo.toml` (`gpu-cuda` / `gpu-metal`). This script
//! therefore: (a) detects the hardware, (b) emits cfgs + env for the crate
//! itself, (c) warns when a toolkit is found but the matching feature is off,
//! and (d) would fail fast with a clear message instead of a linker error if a
//! GPU feature is requested on a machine with no toolchain.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CUDA_PATH");
    println!("cargo:rerun-if-env-changed=CUDA_HOME");
    println!("cargo:rerun-if-env-changed=NVCC");
    println!("cargo:rustc-check-cfg=cfg(ai_accel_cuda)");
    println!("cargo:rustc-check-cfg=cfg(ai_accel_metal)");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    // Must run before any acceleration detection: generates tauri's config.
    tauri_build::build();

    match target_os.as_str() {
        "macos" => {
            println!("cargo:rustc-cfg=ai_accel_metal");
            println!("cargo:rustc-env=AI_ACCEL=metal");
            println!(
                "cargo:warning=GPU: Apple Metal is the default backend on macOS (llama.cpp `metal` feature is enabled in Cargo.toml)."
            );
        }
        _ => detect_cuda(),
    }
}

/// Windows/Linux: locate a CUDA toolkit and validate it against `gpu-cuda`.
fn detect_cuda() {
    let Some(toolkit) = find_cuda_root() else {
        if env::var("CARGO_FEATURE_GPU_CUDA").is_ok() {
            panic!(
                "`gpu-cuda` feature is enabled but no CUDA toolkit was found. \
                 Install the CUDA Toolkit (nvcc) or build without --features gpu-cuda."
            );
        }
        println!("cargo:rustc-env=AI_ACCEL=none");
        println!("cargo:warning=GPU: no CUDA toolkit detected - compiling CPU-only (llama.cpp CPU backend).");
        return;
    };

    println!("cargo:rustc-cfg=ai_accel_cuda");
    println!("cargo:rustc-env=CUDA_PATH={}", toolkit.display());
    println!("cargo:rustc-env=AI_ACCEL=cuda");

    if env::var("CARGO_FEATURE_GPU_CUDA").is_ok() {
        println!(
            "cargo:warning=GPU: CUDA toolkit {} detected and `gpu-cuda` is enabled - compiling llama.cpp with the CUDA backend.",
            toolkit.display()
        );
    } else {
        println!(
            "cargo:warning=GPU: CUDA toolkit detected at {}. To enable the llama.cpp CUDA backend rebuild with `--features gpu-cuda`. Continuing with the CPU-only backend.",
            toolkit.display()
        );
    }
}

/// Best-effort CUDA toolkit discovery, in priority order:
/// 1. `CUDA_PATH` / `CUDA_HOME` env vars (must contain `bin/nvcc*`).
/// 2. `nvcc` resolvable from `PATH` (root = parent of the `bin` dir).
/// 3. Well-known install locations, newest first.
fn find_cuda_root() -> Option<PathBuf> {
    for var in ["CUDA_PATH", "CUDA_HOME"] {
        if let Ok(p) = env::var(var) {
            let cand = PathBuf::from(&p);
            if cand.join("bin").join(nvcc_name()).is_file() {
                return Some(cand);
            }
        }
    }

    if let Some(root) = nvcc_on_path() {
        return Some(root);
    }

    if cfg!(windows) {
        let base = Path::new(r"C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA");
        return newest_cuda_dir(base);
    }

    let p = Path::new("/usr/local/cuda");
    if p.join("bin").join("nvcc").is_file() {
        return Some(p.to_path_buf());
    }
    None
}

fn nvcc_on_path() -> Option<PathBuf> {
    let paths = env::var("PATH").ok()?;
    for dir in env::split_paths(&paths) {
        let cand = dir.join(nvcc_name());
        if cand.is_file() {
            if let Ok(canon) = cand.canonicalize() {
                if let Some(bin) = canon.parent() {
                    if let Some(root) = bin.parent() {
                        return Some(root.to_path_buf());
                    }
                }
            }
        }
    }
    None
}

fn newest_cuda_dir(base: &Path) -> Option<PathBuf> {
    if !base.is_dir() {
        return None;
    }
    let mut versions: Vec<PathBuf> = std::fs::read_dir(base)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.join("bin").join(nvcc_name()).is_file())
        .collect();
    versions.sort_by_key(|p| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    versions.last().cloned()
}

fn nvcc_name() -> &'static str {
    if cfg!(windows) {
        "nvcc.exe"
    } else {
        "nvcc"
    }
}

// Keep `Command` imported (reserved for future introspection like `nvcc --version`).
#[allow(dead_code)]
fn nvcc_version(exe: &Path) -> Option<String> {
    let out = Command::new(exe).arg("--version").output().ok()?;
    String::from_utf8(out.stdout).ok()
}
