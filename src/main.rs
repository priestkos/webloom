// Webloom — a WYSIWYG HTML/CSS editor with a live preview pane.
//
// Architecture: one `tao` window hosting one `wry` webview (WebView2 on
// Windows). The entire user interface is the embedded document in
// assets/index.html; the Rust side owns the window, the native file dialogs
// (rfd) and all file I/O, reached from the page over wry's IPC channel
// (window.ipc.postMessage -> WebViewBuilder::with_ipc_handler).
//
// Theme: the "Kos Mos" palette, taken verbatim from the live source
// D:\Profile\assets\style.css :root. Gold is material (structure, hairlines),
// crimson is the only accent that "lights up".

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};

use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::{Window, WindowBuilder},
};
use wry::{http::Request, WebView, WebViewBuilder, RGBA};

/// The whole front-end, compiled into the executable.
const INDEX_HTML: &str = include_str!("../assets/index.html");

/// Kos Mos ground colour (#07080c), mirrors --bg-deep in the UI so the window
/// paints dark before the webview shows anything.
const GROUND: RGBA = (0x07, 0x08, 0x0c, 0xff);

#[derive(Debug)]
enum UserEvent {
    /// A JSON message posted by the page over `window.ipc.postMessage`.
    Ipc(String),
}

fn main() -> wry::Result<()> {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let window = WindowBuilder::new()
        .with_title("Webloom")
        .with_inner_size(LogicalSize::new(1340.0, 880.0))
        .with_min_inner_size(LogicalSize::new(760.0, 520.0))
        .build(&event_loop)
        .expect("failed to build the Webloom window");

    let proxy = event_loop.create_proxy();
    let ipc_handler = move |req: Request<String>| {
        #[cfg(debug_assertions)]
        eprintln!("webloom: ipc received a message");
        if let Err(err) = proxy.send_event(UserEvent::Ipc(req.body().clone())) {
            eprintln!("webloom: ipc dispatch failed: {err:?}");
        }
    };

    let builder = WebViewBuilder::new()
        .with_html(INDEX_HTML)
        .with_background_color(GROUND)
        .with_devtools(cfg!(debug_assertions))
        .with_ipc_handler(ipc_handler);

    let webview = builder.build(&window)?;
    let mut webview = Some(webview);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                let _ = webview.take();
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(UserEvent::Ipc(body)) => {
                if let Some(wv) = webview.as_ref() {
                    dispatch(&body, wv, &window);
                }
            }
            _ => {}
        }
    });
}

/// Route one JSON message from the page.
fn dispatch(body: &str, webview: &WebView, window: &Window) {
    let msg: serde_json::Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(err) => return reply(webview, error(&format!("malformed ipc message: {err}"))),
    };

    let cmd = msg.get("cmd").and_then(|c| c.as_str()).unwrap_or_default();
    #[cfg(debug_assertions)]
    eprintln!("webloom: dispatch <- {cmd}");
    match cmd {
        "open" => open_document(webview),
        "save" => save_document(&msg, webview),
        "export" => export_document(&msg, webview),
        "set-title" => {
            if let Some(title) = msg.get("title").and_then(|t| t.as_str()) {
                window.set_title(&format!("{title} \u{00b7} Webloom"));
            }
        }
        other => reply(webview, error(&format!("unknown command: {other}"))),
    }
}

/// Show a native open dialog, read the chosen file, hand it back to the page.
fn open_document(webview: &WebView) {
    let picked = rfd::FileDialog::new()
        .add_filter("Web pages", &["html", "htm"])
        .add_filter("All files", &["*"])
        .set_title("Open a document in Webloom")
        .pick_file();

    let Some(path) = picked else {
        return; // user cancelled
    };

    match std::fs::read_to_string(&path) {
        Ok(content) => reply(
            webview,
            serde_json::json!({
                "cmd": "opened",
                "path": path.to_string_lossy(),
                "content": content,
            }),
        ),
        Err(err) => reply(
            webview,
            error(&format!("could not read {}: {err}", path.display())),
        ),
    }
}

/// Save to the page's current path, or prompt for one the first time.
fn save_document(msg: &serde_json::Value, webview: &WebView) {
    let content = msg
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_string();

    let target = msg
        .get("path")
        .and_then(|p| p.as_str())
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            rfd::FileDialog::new()
                .add_filter("Web page", &["html"])
                .set_file_name("untitled.html")
                .set_title("Save document")
                .save_file()
        });

    let Some(path) = target else {
        return; // user cancelled
    };

    write_and_report(webview, &path, &content);
}

/// Always prompt: write a standalone .html the user can open anywhere.
fn export_document(msg: &serde_json::Value, webview: &WebView) {
    let content = msg
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_string();
    let default_name = msg
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("export.html");

    let Some(path) = rfd::FileDialog::new()
        .add_filter("Web page", &["html"])
        .set_file_name(default_name)
        .set_title("Export standalone HTML")
        .save_file()
    else {
        return; // user cancelled
    };

    write_and_report(webview, &path, &content);
}

fn write_and_report(webview: &WebView, path: &Path, content: &str) {
    match std::fs::write(path, content) {
        Ok(()) => reply(
            webview,
            serde_json::json!({ "cmd": "saved", "path": path.to_string_lossy() }),
        ),
        Err(err) => reply(
            webview,
            error(&format!("could not write {}: {err}", path.display())),
        ),
    }
}

fn error(message: &str) -> serde_json::Value {
    serde_json::json!({ "cmd": "error", "message": message })
}

/// Hand a JSON payload to the page's `Webloom.receive` entry point.
fn reply(webview: &WebView, value: serde_json::Value) {
    let js = format!("window.Webloom && window.Webloom.receive({value});");
    if let Err(err) = webview.evaluate_script(&js) {
        eprintln!("webloom: evaluate_script failed: {err:?}");
    }
}
