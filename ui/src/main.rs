#![windows_subsystem = "windows"]
use daisyloop_ui::ipc::{Link, LinkEvent};
use daisyloop_ui::protocol::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;

const BG_BGR: u32 = 0x000C0907;
const TEXT_BGR: u32 = 0x00F4EFEC;

struct Shared {
    link: Link,
    visible: AtomicBool,
    last: Mutex<Option<Snapshot>>,
    alarms: AtomicUsize,
}

#[tauri::command]
fn send_command(shared: State<Arc<Shared>>, cmd: Command) {
    shared.link.command(cmd);
}

#[tauri::command]
fn request_history(shared: State<Arc<Shared>>, tag: u64, query: HistoryQuery) {
    shared.link.history(tag, query);
}

#[tauri::command]
fn last_snapshot(shared: State<Arc<Shared>>) -> Option<Snapshot> {
    shared.last.lock().unwrap().clone()
}

fn reveal(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        let shared = app.state::<Arc<Shared>>();
        shared.visible.store(true, Ordering::Relaxed);
        let _ = app.emit("visibility", true);
        let last = shared.last.lock().unwrap().clone();
        if let Some(s) = last {
            let _ = app.emit("snapshot", s);
        }
    }
}

fn style_caption(w: &tauri::WebviewWindow) {
    if let Ok(h) = w.hwnd() {
        for (attr, color) in [(35u32, BG_BGR), (36, TEXT_BGR), (34, BG_BGR)] {
            unsafe { DwmSetWindowAttribute(h.0 as _, attr as _, &color as *const u32 as *const _, 4) };
        }
    }
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![send_command, request_history, last_snapshot])
        .setup(|app| {
            let handle = app.handle().clone();
            let shared_slot: Arc<Mutex<Option<Arc<Shared>>>> = Arc::new(Mutex::new(None));
            let slot = shared_slot.clone();
            let link = Link::start(move |ev| {
                let Some(shared) = slot.lock().unwrap().clone() else { return };
                let visible = shared.visible.load(Ordering::Relaxed);
                match ev {
                    LinkEvent::Snapshot(s) => {
                        let n = s.alarms.len();
                        if shared.alarms.swap(n, Ordering::Relaxed) != n {
                            if let Some(t) = handle.tray_by_id("main") {
                                let _ = t.set_tooltip(Some(if n > 0 { format!("daisyloop: {n} Alarm(e)") } else { "daisyloop".to_string() }));
                            }
                        }
                        if visible {
                            let _ = handle.emit("snapshot", &s);
                        }
                        *shared.last.lock().unwrap() = Some(s);
                    }
                    LinkEvent::History(tag, h) => {
                        if visible {
                            let _ = handle.emit("history", serde_json::json!({ "tag": tag, "response": h }));
                        }
                    }
                    LinkEvent::Down(e) => {
                        if visible {
                            let _ = handle.emit("link-down", e);
                        }
                    }
                    LinkEvent::Error(e) => {
                        let _ = handle.emit("link-error", e);
                    }
                }
            });
            let shared = Arc::new(Shared { link, visible: AtomicBool::new(true), last: Mutex::new(None), alarms: AtomicUsize::new(0) });
            *shared_slot.lock().unwrap() = Some(shared.clone());
            app.manage(shared);
            let open = MenuItem::with_id(app, "open", "Öffnen", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Beenden", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;
            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("daisyloop")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, ev| match ev.id.as_ref() {
                    "open" => reveal(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } | TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } = ev {
                        reveal(tray.app_handle());
                    }
                })
                .build(app)?;
            if let Some(w) = app.get_webview_window("main") {
                style_caption(&w);
                let _ = w.show();
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                let shared = window.app_handle().state::<Arc<Shared>>();
                shared.visible.store(false, Ordering::Relaxed);
                let _ = window.app_handle().emit("visibility", false);
            }
        })
        .run(tauri::generate_context!())
        .expect("tauri");
}
