#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// Bundled app: the sidecar sits next to the main executable.
/// Dev (`cargo run`): fall back to the monorepo build output.
fn server_binary() -> PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    if let Some(dir) = exe_dir {
        let bundled = dir.join("zennotes-server");
        if bundled.exists() {
            return bundled;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../server/bin/zennotes-server")
}

/// Bundled app: Contents/Resources/tikz-runtime. Dev: the monorepo dir.
fn tikz_runtime_dir() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    {
        candidates.push(dir.join("../Resources/tikz-runtime"));
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tikz-runtime"));
    candidates
        .into_iter()
        .find(|d| d.join("tikz-server.mjs").exists())
}

/// GUI apps get a minimal PATH on macOS, so PATH lookup alone won't find a
/// Homebrew node; probe the usual install locations too.
fn find_node() -> Option<PathBuf> {
    for p in ["/opt/homebrew/bin/node", "/usr/local/bin/node", "/usr/bin/node"] {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    let ok = Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    ok.then(|| PathBuf::from("node"))
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}

/// The webview's localStorage (onboarding done, theme, vim prefs…) is scoped
/// to the origin, port included — so the UI port must be STABLE across
/// launches or every start looks like a first run. Fixed port with a few
/// sequential fallbacks; a fallback launch works but sees fresh prefs.
fn stable_ui_port() -> u16 {
    if let Some(p) = std::env::var("ZENNOTES_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
    {
        return p;
    }
    for port in 39420..39430 {
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
    free_port()
}

fn vault_path() -> PathBuf {
    std::env::var_os("ZENNOTES_VAULT_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            #[allow(deprecated)]
            std::env::home_dir()
                .expect("no home directory")
                .join("ZenNotes")
        })
}

fn main() {
    let port = stable_ui_port();
    let bind = format!("127.0.0.1:{port}");

    let vault = vault_path();
    std::fs::create_dir_all(&vault).expect("create vault dir");

    // TikZ renders in a small Node process (wasm TeX engine the Go server
    // can't host). Best-effort: without node the server simply doesn't
    // advertise supportsTikz and the UI shows the friendly error.
    let mut tikz = None;
    let mut tikz_upstream = String::new();
    if let (Some(node), Some(dir)) = (find_node(), tikz_runtime_dir()) {
        let tikz_port = free_port();
        match Command::new(node)
            .arg(dir.join("tikz-server.mjs"))
            .env("TIKZ_PORT", tikz_port.to_string())
            .spawn()
        {
            Ok(child) => {
                tikz = Some(child);
                tikz_upstream = format!("http://127.0.0.1:{tikz_port}");
            }
            Err(err) => eprintln!("tikz render process failed to start: {err}"),
        }
    }

    // The host config (~/.zennotes/server.json) persists the last selected
    // vault across launches; the default path only seeds the first run.
    // ZENNOTES_VAULT_PATH is left to the user (inherited env) since it
    // hard-locks vault selection (409 on /vault/select).
    let mut server = Command::new(server_binary())
        .env("ZENNOTES_BIND", &bind)
        .env("ZENNOTES_DEFAULT_VAULT_PATH", &vault)
        .env("ZENNOTES_ALLOW_UNSCOPED_BROWSE", "1")
        .env("ZENNOTES_TIKZ_UPSTREAM", &tikz_upstream)
        .spawn()
        .expect("failed to start zennotes-server");

    // ponytail: TCP-connect polling as the readiness check, switch to a real
    // /health endpoint if the server ever binds before it can serve.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while TcpStream::connect(&bind).is_err() {
        if std::time::Instant::now() > deadline {
            let _ = server.kill();
            panic!("zennotes-server did not become ready on {bind}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let url: tauri::Url = format!("http://{bind}").parse().expect("url");

    tauri::Builder::default()
        .setup(move |app| {
            let origin = format!("http://{bind}");
            let handle = app.handle().clone();
            let popup_seq = std::sync::atomic::AtomicU32::new(0);
            tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::External(url.clone()))
                .title("ZenNotes")
                .inner_size(1280.0, 840.0)
                // window.open: same-origin popups (the PDF export window)
                // become real Tauri windows; anything else goes to the system
                // browser. WKWebView has no window.print, so the popup gets a
                // shim that signals through the title and Rust runs the
                // native print operation ("Save as PDF" lives in its dialog).
                .on_new_window(move |popup_url, features| {
                    if !popup_url.as_str().starts_with(&origin) {
                        let _ = Command::new("open").arg(popup_url.as_str()).spawn();
                        return tauri::webview::NewWindowResponse::Deny;
                    }
                    let label = format!(
                        "popup-{}",
                        popup_seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    );
                    let built = tauri::WebviewWindowBuilder::new(
                        &handle,
                        label,
                        tauri::WebviewUrl::External(popup_url),
                    )
                    .window_features(features)
                    .initialization_script(
                        "window.print = () => { document.title = '__zn_print__' };",
                    )
                    .on_document_title_changed(|window, title| {
                        if title == "__zn_print__" {
                            let _ = window.print();
                        }
                    })
                    .title("ZenNotes — Export")
                    .inner_size(900.0, 800.0)
                    .build();
                    match built {
                        Ok(window) => tauri::webview::NewWindowResponse::Create { window },
                        Err(err) => {
                            eprintln!("popup window failed: {err}");
                            tauri::webview::NewWindowResponse::Deny
                        }
                    }
                })
                .build()?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error building tauri app")
        .run(move |_app, event| {
            if let tauri::RunEvent::Exit = event {
                let _ = server.kill();
                if let Some(child) = tikz.as_mut() {
                    let _ = child.kill();
                }
            }
        });
}
