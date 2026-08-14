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

/// Destination for a PDF export: ~/Downloads/<note title>.pdf, numbered
/// instead of overwriting. The exportNote query param is the vault-relative
/// note path, already percent-decoded by the Url parser.
fn pdf_destination(url: &tauri::Url) -> PathBuf {
    let note = url
        .query_pairs()
        .find(|(k, _)| k == "exportNote")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| "note".into());
    let stem = note
        .rsplit('/')
        .next()
        .unwrap_or(&note)
        .trim_end_matches(".md");
    #[allow(deprecated)]
    let downloads = std::env::home_dir()
        .expect("no home directory")
        .join("Downloads");
    let mut dest = downloads.join(format!("{stem}.pdf"));
    let mut n = 2;
    while dest.exists() {
        dest = downloads.join(format!("{stem} ({n}).pdf"));
        n += 1;
    }
    dest
}

/// Paginated PDF straight to disk — the WKWebView equivalent of Electron's
/// printToPDF: a print operation with the panel disabled and the job
/// disposition set to "save to this URL". Must run on the main thread, and
/// MUST use the modal runner: Apple documents the synchronous runOperation
/// as unsupported for WKWebView (it produced a runaway million-page PDF).
/// The modal variant returns immediately; completion is the file appearing.
#[cfg(target_os = "macos")]
fn silent_print_to_pdf(wk: &objc2_web_kit::WKWebView, dest: &std::path::Path) -> bool {
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{NSPrintInfo, NSPrintJobSavingURL, NSPrintSaveJob};
    use objc2_foundation::{NSString, NSURL};
    unsafe {
        let Some(window) = wk.window() else {
            return false;
        };
        // Mutating the shared print info is what wry's own print() does; we
        // never run another print job that would care about the leftovers.
        let info = NSPrintInfo::sharedPrintInfo();
        info.setJobDisposition(NSPrintSaveJob);
        let url = NSURL::fileURLWithPath(&NSString::from_str(&dest.to_string_lossy()));
        info.dictionary()
            .setObject_forKey(&url, ProtocolObject::from_ref(NSPrintJobSavingURL));
        let op = wk.printOperationWithPrintInfo(&info);
        op.setShowsPrintPanel(false);
        op.setShowsProgressPanel(false);
        op.setCanSpawnSeparateThread(true);
        op.runOperationModalForWindow_delegate_didRunSelector_contextInfo(
            &window,
            None,
            None,
            std::ptr::null_mut(),
        );
        true
    }
}

/// Wait for the print job to finish writing `dest` (existence + stable size),
/// then reveal it in Finder and close the export window.
fn reveal_when_written(dest: PathBuf, win: tauri::WebviewWindow) {
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut last_size = None;
        while std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(300));
            match std::fs::metadata(&dest) {
                Ok(meta) if Some(meta.len()) == last_size && meta.len() > 0 => {
                    let _ = Command::new("open").arg("-R").arg(&dest).spawn();
                    let _ = win.close();
                    return;
                }
                Ok(meta) => last_size = Some(meta.len()),
                Err(_) => {}
            }
        }
        eprintln!("[popup] print job never produced {dest:?}");
        let _ = win.close();
    });
}

/// A popup window for a same-origin window.open. Export popups stay hidden
/// and print themselves to a PDF; regular popups behave like small windows.
fn spawn_export_popup(
    handle: &tauri::AppHandle,
    popup_url: tauri::Url,
    features: Option<tauri::webview::NewWindowFeatures>,
    label: String,
) -> tauri::Result<tauri::WebviewWindow> {
    let is_export = popup_url.query_pairs().any(|(k, _)| k == "exportNote");
    let dest = pdf_destination(&popup_url);
    let mut builder =
        tauri::WebviewWindowBuilder::new(handle, label, tauri::WebviewUrl::External(popup_url))
            .visible(!is_export)
            // Two shims: window.print does not exist in WKWebView (signal
            // through the title instead), and requestAnimationFrame never
            // fires in a hidden window — the export page waits on it before
            // printing, so route it through setTimeout.
            .initialization_script(
                "window.print = () => { document.title = '__zn_print__' };\n\
                 window.requestAnimationFrame = (cb) => setTimeout(() => cb(performance.now()), 16);",
            )
            .on_document_title_changed(move |window, title| {
                if title != "__zn_print__" {
                    return;
                }
                let dest = dest.clone();
                let win = window.clone();
                let _ = window.with_webview(move |pw| {
                    #[cfg(target_os = "macos")]
                    {
                        let wk = unsafe { &*pw.inner().cast::<objc2_web_kit::WKWebView>() };
                        let ok = silent_print_to_pdf(wk, &dest);
                        eprintln!("[popup] print job started for {dest:?}: {ok}");
                        if ok {
                            reveal_when_written(dest, win);
                        } else {
                            let _ = win.close();
                        }
                    }
                    #[cfg(not(target_os = "macos"))]
                    let _ = win.close();
                });
            })
            .title("ZenNotes — Export")
            .inner_size(900.0, 800.0);
    if let Some(features) = features {
        builder = builder.window_features(features);
    }
    builder.build()
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
                // window.open: same-origin popups become real Tauri windows;
                // anything else goes to the system browser. The PDF export
                // popup mirrors the Electron desktop: a HIDDEN window renders
                // the note, then a panel-less native print operation writes
                // the paginated PDF to ~/Downloads (WKWebView has no
                // window.print — an injected shim signals readiness through
                // the title, exactly when the page would have printed).
                .on_new_window(move |popup_url, features| {
                    if !popup_url.as_str().starts_with(&origin) {
                        let _ = Command::new("open").arg(popup_url.as_str()).spawn();
                        return tauri::webview::NewWindowResponse::Deny;
                    }
                    let label = format!(
                        "popup-{}",
                        popup_seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    );
                    match spawn_export_popup(&handle, popup_url, Some(features), label) {
                        Ok(window) => tauri::webview::NewWindowResponse::Create { window },
                        Err(err) => {
                            eprintln!("popup window failed: {err}");
                            tauri::webview::NewWindowResponse::Deny
                        }
                    }
                })
                .build()?;

            // Test hook: render + silently print one note on launch, no UI
            // driving needed. `ZENNOTES_TEST_EXPORT=inbox/Welcome.md`.
            if let Ok(note) = std::env::var("ZENNOTES_TEST_EXPORT") {
                let export_url: tauri::Url =
                    format!("http://{bind}/?exportNote={note}").parse().expect("test url");
                spawn_export_popup(app.handle(), export_url, None, "test-export".into())?;
            }
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
