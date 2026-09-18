use serde::Serialize;
use std::collections::HashMap;
#[cfg(not(target_os = "android"))]
use std::io::{BufRead, BufReader, Read};
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
#[cfg(not(target_os = "android"))]
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use tauri::Manager;
use tauri::Emitter;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

fn nscb_binary_name() -> &'static str {
    if cfg!(target_os = "android") {
        "embedded-nscb"
    } else if cfg!(target_os = "windows") {
        "nscb_rust.exe"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "nscb_rust-macos-arm64"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "nscb_rust-macos-amd64"
    } else {
        "nscb_rust-linux-amd64"
    }
}

fn app_root_dir() -> Result<std::path::PathBuf, String> {
    #[cfg(debug_assertions)]
    {
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        if let Some(parent) = manifest_dir.parent() {
            return Ok(parent.to_path_buf());
        }
        return Ok(manifest_dir);
    }

    #[cfg(not(debug_assertions))]
    {
        let exe = std::env::current_exe()
            .map_err(|e| format!("Failed to resolve current executable path: {e}"))?;
        if let Some(parent) = exe.parent() {
            return Ok(parent.to_path_buf());
        }
        Err("Failed to resolve executable directory".to_string())
    }
}

fn app_tools_dir(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    let temp_dir = app
        .path()
        .temp_dir()
        .map_err(|e| format!("Failed to resolve temp dir: {e}"))?;
    let tools_dir = temp_dir.join("nscb-desktop-tools");
    std::fs::create_dir_all(&tools_dir)
        .map_err(|e| format!("Failed to create tools dir: {e}"))?;
    Ok(tools_dir)
}

fn nscb_exe_path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    let tools_dir = app_tools_dir(app)?;
    let name = nscb_binary_name();
    let exe_path = tools_dir.join(name);
    if !exe_path.exists() {
        return Err(format!("{} not found at {}", name, exe_path.display()));
    }
    Ok(exe_path)
}

fn running_pid() -> &'static Mutex<Option<u32>> {
    static PID: OnceLock<Mutex<Option<u32>>> = OnceLock::new();
    PID.get_or_init(|| Mutex::new(None))
}

#[derive(Serialize, Clone)]
struct StdoutEvent {
    op: String,
    line: String,
}

#[derive(Serialize, Clone)]
struct StderrEvent {
    op: String,
    chunk: String,
}

#[derive(Serialize, Clone)]
struct DoneEvent {
    op: String,
    code: i32,
}

#[tauri::command]
fn import_keys(app: tauri::AppHandle, data: Vec<u8>) -> Result<(), String> {
    let tools_dir = app_tools_dir(&app)?;

    let dst_prod = tools_dir.join("prod.keys");
    std::fs::write(&dst_prod, data).map_err(|e| format!("Failed to write prod.keys: {e}"))?;
    Ok(())
}

#[tauri::command]
fn get_tools_dir(app: tauri::AppHandle) -> Result<String, String> {
    let tools_dir = app_tools_dir(&app)?;
    Ok(tools_dir.to_string_lossy().into_owned())
}

#[tauri::command]
fn has_keys(app: tauri::AppHandle) -> Result<bool, String> {
    let tools_dir = app_tools_dir(&app)?;
    Ok(tools_dir.join("prod.keys").exists() || tools_dir.join("keys.txt").exists())
}

#[tauri::command]
fn has_backend(app: tauri::AppHandle) -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        let _ = app;
        return Ok(true);
    }
    #[cfg(not(target_os = "android"))]
    {
    let tools_dir = app_tools_dir(&app)?;
    Ok(tools_dir.join(nscb_binary_name()).exists())
    }
}

#[tauri::command]
fn import_nscb_binary(app: tauri::AppHandle, src_path: String) -> Result<(), String> {
    let src = std::path::PathBuf::from(&src_path);
    if !src.exists() {
        return Err("Selected file does not exist".to_string());
    }
    let filename = src
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !filename.starts_with("nscb_rust") {
        return Err("Please select an nscb_rust backend binary".to_string());
    }

    let tools_dir = app_tools_dir(&app)?;
    let dst = tools_dir.join(nscb_binary_name());
    std::fs::copy(src, &dst).map_err(|e| format!("Failed to copy backend binary: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755)).ok();
    }

    Ok(())
}

#[tauri::command]
#[cfg(not(target_os = "android"))]
fn run_nscb(app: tauri::AppHandle, operation: String, args: Vec<String>) -> Result<(), String> {
    {
        let mut lock = running_pid()
            .lock()
            .map_err(|_| "Failed to lock runner state".to_string())?;
        if lock.is_some() {
            return Err("A process is already running".to_string());
        }

        let exe_path = nscb_exe_path(&app)?;
        let work_dir = app_root_dir()?;

        let mut cmd = Command::new(exe_path);
        cmd.args(args)
            .current_dir(work_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(target_os = "windows")]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let mut child = cmd.spawn()
            .map_err(|e| format!("Failed to start {}: {e}", nscb_binary_name()))?;

        *lock = Some(child.id());

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Failed to capture stdout".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "Failed to capture stderr".to_string())?;

        let app_for_out = app.clone();
        let op_for_out = operation.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            let _ = app_for_out.emit(
                                "nscb-stdout",
                                StdoutEvent {
                                    op: op_for_out.clone(),
                                    line: trimmed.to_string(),
                                },
                            );
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let app_for_err = app.clone();
        let op_for_err = operation.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut buf = [0_u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                        if !chunk.trim().is_empty() {
                            let _ = app_for_err.emit(
                                "nscb-stderr",
                                StderrEvent {
                                    op: op_for_err.clone(),
                                    chunk,
                                },
                            );
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let op_for_done = operation.clone();
        std::thread::spawn(move || {
            let code = match child.wait() {
                Ok(status) => status.code().unwrap_or(-1),
                Err(_) => -1,
            };
            if let Ok(mut pid_lock) = running_pid().lock() {
                *pid_lock = None;
            }
            let _ = app.emit(
                "nscb-done",
                DoneEvent {
                    op: op_for_done,
                    code,
                },
            );
        });
    }

    Ok(())
}

#[tauri::command]
#[cfg(target_os = "android")]
fn run_nscb(app: tauri::AppHandle, operation: String, args: Vec<String>) -> Result<(), String> {
    use clap::Parser;
    use std::hash::{DefaultHasher, Hash, Hasher};
    use std::io::{Read, Seek, SeekFrom, Write};
    use tauri_plugin_fs::{FsExt, OpenOptions};

    {
        let mut lock = running_pid()
            .lock()
            .map_err(|_| "Failed to lock runner state".to_string())?;
        if lock.is_some() {
            return Err("An operation is already running".to_string());
        }
        *lock = Some(0);
    }

    std::thread::spawn(move || {
        let _ = app.emit(
            "nscb-stdout",
            StdoutEvent {
                op: operation.clone(),
                line: "[START] Running embedded Android backend".to_string(),
            },
        );

        let resolved_args = args
            .into_iter()
            .map(|arg| {
                if !arg.starts_with("content://") {
                    return Ok(arg);
                }

                let uri = tauri::Url::parse(&arg)
                    .map_err(|error| format!("Invalid Android document URI: {error}"))?;
                let mut options = OpenOptions::new();
                options.read(true);
                let mut source = app
                    .fs()
                    .open(uri, options)
                    .map_err(|error| format!("Failed to open Android document: {error}"))?;
                let source_size = source.metadata().ok().map(|metadata| metadata.len());

                let raw_name = arg
                    .rsplit_once("%2F")
                    .or_else(|| arg.rsplit_once("%2f"))
                    .map(|(_, name)| name)
                    .or_else(|| arg.rsplit_once('/').map(|(_, name)| name))
                    .unwrap_or("android-input.bin");
                let safe_name: String = raw_name
                    .replace("%20", "_")
                    .chars()
                    .map(|ch| if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') { ch } else { '_' })
                    .collect();
                let mut hasher = DefaultHasher::new();
                arg.hash(&mut hasher);
                let cache_path = app_tools_dir(&app)?
                    .join("android-native-inputs")
                    .join(format!("{:016x}-{safe_name}", hasher.finish()));
                if let Some(parent) = cache_path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|error| format!("Failed to create Android input cache: {error}"))?;
                }

                let cached_size = std::fs::metadata(&cache_path).ok().map(|metadata| metadata.len());
                if source_size.is_some() && source_size == cached_size {
                    let _ = app.emit(
                        "nscb-stdout",
                        StdoutEvent {
                            op: operation.clone(),
                            line: format!("[ANDROID] Reusing native cached copy of {safe_name}: 99%. Starting NSCB processing..."),
                        },
                    );
                    return Ok(cache_path.to_string_lossy().into_owned());
                }

                let _ = app.emit(
                    "nscb-stdout",
                    StdoutEvent {
                        op: operation.clone(),
                        line: format!("[ANDROID] Preparing {safe_name} with native I/O"),
                    },
                );
                source
                    .seek(SeekFrom::Start(0))
                    .map_err(|error| format!("Failed to seek Android document: {error}"))?;
                let mut destination = std::fs::File::create(&cache_path)
                    .map_err(|error| format!("Failed to create Android input cache file: {error}"))?;
                let mut buffer = vec![0_u8; 16 * 1024 * 1024];
                let mut copied = 0_u64;
                let mut last_percent = 0_u64;
                loop {
                    let count = source
                        .read(&mut buffer)
                        .map_err(|error| format!("Failed to read Android document: {error}"))?;
                    if count == 0 {
                        break;
                    }
                    destination
                        .write_all(&buffer[..count])
                        .map_err(|error| format!("Failed to cache Android document: {error}"))?;
                    copied += count as u64;

                    if let Some(total) = source_size.filter(|total| *total > 0) {
                        let percent = (copied.saturating_mul(100) / total).min(99);
                        if percent > last_percent {
                            last_percent = percent;
                            let _ = app.emit(
                                "nscb-stdout",
                                StdoutEvent {
                                    op: operation.clone(),
                                    line: format!(
                                        "[ANDROID] Native preparation: {percent}% ({} / {} MiB)",
                                        copied / 1_048_576,
                                        total / 1_048_576
                                    ),
                                },
                            );
                        }
                    }
                }
                destination
                    .flush()
                    .map_err(|error| format!("Failed to finish Android input cache: {error}"))?;
                let _ = app.emit(
                    "nscb-stdout",
                    StdoutEvent {
                        op: operation.clone(),
                        line: format!(
                            "[ANDROID] Native preparation complete: 99% ({} MiB). Starting NSCB processing...",
                            copied / 1_048_576
                        ),
                    },
                );

                Ok(cache_path.to_string_lossy().into_owned())
            })
            .collect::<Result<Vec<_>, String>>();

        let result = resolved_args.and_then(|resolved_args| {
            let _ = app.emit(
                "nscb-stdout",
                StdoutEvent {
                    op: operation.clone(),
                    line: "[PROCESSING] NSCB is reading and analyzing the prepared file...".to_string(),
                },
            );
            let processing = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
            let heartbeat_flag = processing.clone();
            let heartbeat_app = app.clone();
            let heartbeat_operation = operation.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(10));
                if !heartbeat_flag.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                let _ = heartbeat_app.emit(
                    "nscb-stdout",
                    StdoutEvent {
                        op: heartbeat_operation.clone(),
                        line: "[PROCESSING] NSCB is still working...".to_string(),
                    },
                );
            });
            let mut argv = vec!["nscb".to_string()];
            argv.extend(resolved_args);
            let dispatch_result = nscb::cli::Args::try_parse_from(argv)
                .map_err(|error| error.to_string())
                .and_then(|parsed| nscb::cli::dispatch(parsed).map_err(|error| error.to_string()));
            processing.store(false, std::sync::atomic::Ordering::Relaxed);
            dispatch_result
        });

        let code = if let Err(message) = result {
            let _ = app.emit(
                "nscb-stderr",
                StderrEvent {
                    op: operation.clone(),
                    chunk: message,
                },
            );
            1
        } else {
            let _ = app.emit(
                "nscb-stdout",
                StdoutEvent {
                    op: operation.clone(),
                    line: "[DONE] Android operation completed".to_string(),
                },
            );
            0
        };

        if let Ok(mut lock) = running_pid().lock() {
            *lock = None;
        }
        let _ = app.emit("nscb-done", DoneEvent { op: operation, code });
    });

    Ok(())
}

#[tauri::command]
fn get_backend_version(app: tauri::AppHandle) -> Result<String, String> {
    let settings = read_settings(&app)?;
    if let Some(v) = settings.get("backendVersion") {
        return Ok(v.clone());
    }
    // migrate from legacy version.txt
    let tools_dir = app_tools_dir(&app)?;
    let version_file = tools_dir.join("version.txt");
    if version_file.exists() {
        let v = std::fs::read_to_string(&version_file)
            .map(|s| s.trim().to_string())
            .map_err(|e| format!("Failed to read version: {e}"))?;
        if !v.is_empty() {
            let mut settings = settings;
            settings.insert("backendVersion".to_string(), v.clone());
            write_settings(&app, &settings)?;
        }
        let _ = std::fs::remove_file(&version_file);
        return Ok(v);
    }
    Ok(String::new())
}

#[tauri::command]
fn save_backend_version(app: tauri::AppHandle, version: String) -> Result<(), String> {
    save_setting(app, "backendVersion".to_string(), version)
}

#[tauri::command]
fn download_backend(app: tauri::AppHandle, url: String) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        let _ = (app, url);
        return Ok(());
    }
    #[cfg(not(target_os = "android"))]
    {
    let tools_dir = app_tools_dir(&app)?;
    let dst = tools_dir.join(nscb_binary_name());

    let response = reqwest::blocking::get(&url)
        .map_err(|e| format!("Download failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("Download returned HTTP {}", response.status()));
    }
    let bytes = response
        .bytes()
        .map_err(|e| format!("Failed to read response body: {e}"))?;
    std::fs::write(&dst, &bytes)
        .map_err(|e| format!("Failed to save backend binary: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755)).ok();
    }

    Ok(())
    }
}

#[tauri::command]
fn create_verify_filelist(app: tauri::AppHandle, target_path: String) -> Result<String, String> {
    let tools_dir = app_tools_dir(&app)?;
    let stem = std::path::Path::new(&target_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("verify");
    let safe_stem: String = stem.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let list_name = format!("{}-vflist.txt", safe_stem);
    let list_path = tools_dir.join(&list_name);

    // Always delete the cached result so the backend runs fresh
    let cached = tools_dir.join("INFO").join("MASSVERIFY")
        .join(format!("{}-vflist-verify.txt", safe_stem));
    let _ = std::fs::remove_file(&cached);

    std::fs::write(&list_path, format!("{}\n", target_path))
        .map_err(|e| format!("Failed to create verify filelist: {e}"))?;
    Ok(list_path.to_string_lossy().into_owned())
}

#[tauri::command]
fn read_file_text(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read file: {e}"))
}

#[tauri::command]
fn cancel_nscb() -> Result<(), String> {
    let pid_opt = {
        let mut lock = running_pid()
            .lock()
            .map_err(|_| "Failed to lock runner state".to_string())?;
        lock.take()
    };

    if let Some(pid) = pid_opt {
        #[cfg(target_os = "android")]
        {
            let _ = pid;
            return Err("The embedded Android backend cannot be interrupted safely yet".to_string());
        }
        #[cfg(target_os = "windows")]
        {
            let mut cmd = Command::new("taskkill");
            cmd.args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            cmd.creation_flags(CREATE_NO_WINDOW);
            let status = cmd.status()
                .map_err(|e| format!("Failed to stop process: {e}"))?;
            if !status.success() {
                return Err("Failed to stop running process".to_string());
            }
        }

        #[cfg(all(not(target_os = "windows"), not(target_os = "android")))]
        {
            Command::new("kill")
                .args(["-9", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|e| format!("Failed to stop process: {e}"))?;
        }
    }

    Ok(())
}

fn settings_path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    let tools_dir = app_tools_dir(app)?;
    Ok(tools_dir.join("settings.json"))
}

fn read_settings(app: &tauri::AppHandle) -> Result<HashMap<String, String>, String> {
    let path = settings_path(app)?;
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let data = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read settings: {e}"))?;
    serde_json::from_str(&data)
        .map_err(|e| format!("Failed to parse settings: {e}"))
}

fn write_settings(app: &tauri::AppHandle, settings: &HashMap<String, String>) -> Result<(), String> {
    let path = settings_path(app)?;
    let data = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("Failed to serialize settings: {e}"))?;
    std::fs::write(&path, data.as_bytes())
        .map_err(|e| format!("Failed to write settings: {e}"))
}

#[tauri::command]
fn get_setting(app: tauri::AppHandle, key: String) -> Result<String, String> {
    let settings = read_settings(&app)?;
    Ok(settings.get(&key).cloned().unwrap_or_default())
}

#[tauri::command]
fn save_setting(app: tauri::AppHandle, key: String, value: String) -> Result<(), String> {
    let mut settings = read_settings(&app)?;
    if value.is_empty() {
        settings.remove(&key);
    } else {
        settings.insert(key, value);
    }
    write_settings(&app, &settings)
}

#[tauri::command]
fn get_platform() -> String {
    if cfg!(target_os = "android") {
        "android-arm64".to_string()
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "macos-arm64".to_string()
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "macos-amd64".to_string()
    } else {
        std::env::consts::OS.to_string()
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            import_keys,
            import_nscb_binary,
            get_tools_dir,
            has_keys,
            has_backend,
            get_backend_version,
            save_backend_version,
            download_backend,
            run_nscb,
            cancel_nscb,
            create_verify_filelist,
            read_file_text,
            get_platform,
            get_setting,
            save_setting
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
