//! Runs the real app without a display, against a data folder, follows a
//! script of steps and writes screenshots. For reviewing the interface with
//! real mail without touching the desktop.
//!
//!   LOOKOUT_DATA_DIR=<copy of the data folder> \
//!     cargo run -p lookout --example live -- <out dir> <step>...
//!
//! Steps: `size:W,H`, `scale:F`, `wait:MS` (lets the engine work), `shot:NAME`,
//! `page:mail|calendar|people`, `folder:NAME` (first folder with that name), `fav:NAME`, `collapse:NAME`,
//! `select:N` (message), `ctrl-select:N`, `shift-select:N`, `person:N`, `key:NAME[+ctrl][+shift]`, `click:X,Y`, `rclick:X,Y`, `drag:X1,Y1,X2,Y2`,
//! `search:TEXT`, `new-folder:[PARENT/]NAME`, `delete-folder:NAME`, `tab:focused|other`,
//! `focus:N,focused|other[,always]`, `hover:X,Y`, `wheel:X,Y,DY` (or `X,Y,DX,DY`), `mode:N` (calendar view), `next`, `previous`, `dark`.

use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lookout::{App, Cal, Mail, MainWindow, Page, People, Shell};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{EventLoopProxy, Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, EventLoopError, LogicalPosition, Model, PhysicalSize, PlatformError};

type Queue = Arc<Mutex<VecDeque<Box<dyn FnOnce() + Send>>>>;

struct Headless {
    window: Rc<MinimalSoftwareWindow>,
    queue: Queue,
}

struct Proxy(Queue);

impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), EventLoopError> {
        Ok(())
    }

    fn invoke_from_event_loop(&self, event: Box<dyn FnOnce() + Send>) -> Result<(), EventLoopError> {
        self.0.lock().unwrap().push_back(event);
        Ok(())
    }
}

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy(self.queue.clone())))
    }
}

/// Runs queued work and timers for a while.
fn pump(queue: &Queue, time: Duration) {
    let until = Instant::now() + time;
    loop {
        let jobs: Vec<_> = queue.lock().unwrap().drain(..).collect();
        for job in jobs {
            job();
        }
        slint::platform::update_timers_and_animations();
        if Instant::now() >= until {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn click(ui: &MainWindow, x: f32, y: f32, button: PointerEventButton) {
    let position = LogicalPosition::new(x, y);
    for event in [
        WindowEvent::PointerMoved { position },
        WindowEvent::PointerPressed { position, button },
        WindowEvent::PointerReleased { position, button },
    ] {
        ui.window().dispatch_event(event);
    }
}

/// With `LIVE_WINDOWED` set: a real window, the steps run on timers, and the
/// window stays up for the last step's wait. For what only a real window
/// shows, such as WebView2 or partial repaints. Takes `wait:MS`, `select:N`, `page:NAME`,
/// `wheel:X,Y,DY`, `hover:X,Y`, `click:X,Y`.
fn windowed(steps: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let ui = MainWindow::new()?;
    let app = App::start(&ui)?;
    let weak = ui.as_weak();
    fn run(steps: Rc<Vec<String>>, at: usize, ui: slint::Weak<MainWindow>) {
        let Some(step) = steps.get(at) else {
            _ = slint::quit_event_loop();
            return;
        };
        let (name, value) = step.split_once(':').unwrap_or((step.as_str(), ""));
        let mut delay = 200;
        if let Some(ui) = ui.upgrade() {
            match name {
                "wait" => delay = value.parse().unwrap_or(1000),
                "select" => ui.global::<Mail>().invoke_select(value.parse().unwrap_or(0), false, false),
                "page" => {
                    let page = if value == "calendar" { Page::Calendar } else { Page::Mail };
                    ui.global::<Shell>().set_page(page);
                }
                "wheel" | "hover" | "click" => {
                    let numbers: Vec<f32> = value.split(',').filter_map(|n| n.parse().ok()).collect();
                    if let [x, y, ..] = numbers[..] {
                        let position = LogicalPosition::new(x, y);
                        let window = ui.window();
                        window.dispatch_event(WindowEvent::PointerMoved { position });
                        if name == "wheel" {
                            let delta_y = numbers.get(2).copied().unwrap_or(-200.0);
                            window.dispatch_event(WindowEvent::PointerScrolled { position, delta_x: 0.0, delta_y });
                        } else if name == "click" {
                            click(&ui, x, y, PointerEventButton::Left);
                        }
                    }
                }
                _ => {}
            }
        }
        slint::Timer::single_shot(Duration::from_millis(delay), move || run(steps, at + 1, ui));
    }
    let steps = Rc::new(steps);
    slint::Timer::single_shot(Duration::from_millis(100), move || run(steps, 0, weak));
    ui.run()?;
    drop(app);
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let out = std::path::PathBuf::from(args.next().ok_or("usage: live <out dir> <step>...")?);
    if std::env::var_os("LIVE_WINDOWED").is_some() {
        return windowed(args.collect());
    }
    std::fs::create_dir_all(&out)?;
    let queue: Queue = Arc::default();
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless { window: window.clone(), queue: queue.clone() }))?;
    let ui = MainWindow::new()?;
    window.set_size(PhysicalSize::new(1400, 900));
    let app = App::start(&ui)?;
    ui.show()?;
    pump(&queue, Duration::from_millis(500));

    for step in args {
        let (name, value) = step.split_once(':').unwrap_or((step.as_str(), ""));
        match name {
            "size" => {
                let (w, h) = value.split_once(',').ok_or("size:W,H")?;
                window.set_size(PhysicalSize::new(w.parse()?, h.parse()?));
            }
            "scale" => ui.window().dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: value.parse()? }),
            "wait" => pump(&queue, Duration::from_millis(value.parse()?)),
            "shot" => {
                pump(&queue, Duration::from_millis(50));
                let snapshot = ui.window().take_snapshot()?;
                let path = out.join(format!("{value}.png"));
                image::save_buffer(&path, snapshot.as_bytes(), snapshot.width(), snapshot.height(), image::ColorType::Rgba8)?;
                println!("wrote {}", path.display());
            }
            "page" => {
                let page = match value {
                    "calendar" => Page::Calendar,
                    "people" => Page::People,
                    _ => Page::Mail,
                };
                let shell = ui.global::<Shell>();
                shell.set_page(page);
                shell.invoke_page_changed(page);
            }
            "folder" => {
                let mail = ui.global::<Mail>();
                let folders = mail.get_folders();
                let found = folders.iter().find(|f| !f.header && f.name.eq_ignore_ascii_case(value)).ok_or("no such folder")?;
                mail.invoke_folder_clicked(found.account, found.id);
            }
            "fav" | "collapse" => {
                let mail = ui.global::<Mail>();
                let folders = mail.get_folders();
                let found = folders.iter().find(|f| !f.header && f.name.eq_ignore_ascii_case(value)).ok_or("no such folder")?;
                if name == "fav" {
                    mail.invoke_toggle_favourite(found.account, found.id);
                } else {
                    mail.invoke_toggle_expanded(found.account, found.id);
                }
            }
            "new-folder" | "delete-folder" => {
                // As the folder pane's menu and the dialog after it do.
                let (parent, folder) = match value.rsplit_once('/') {
                    Some((parent, folder)) => (Some(parent), folder),
                    None => (None, value),
                };
                let lookup = if name == "new-folder" { parent } else { Some(folder) };
                let account = app.store.accounts()?.first().ok_or("no account")?.id.clone();
                let id = match lookup {
                    Some(wanted) => {
                        let folders = app.store.folders(&account)?;
                        folders.into_iter().find(|f| f.name.eq_ignore_ascii_case(wanted)).ok_or("no such folder")?.id
                    }
                    None => String::new(),
                };
                let mail = ui.global::<Mail>();
                if name == "new-folder" {
                    mail.invoke_new_folder(account.into(), id.into());
                    mail.invoke_folder_dialog_accepted(folder.into());
                } else {
                    mail.invoke_delete_folder(account.into(), id.into());
                    mail.invoke_folder_dialog_accepted("".into());
                }
            }
            "tab" => ui.global::<Mail>().invoke_show_focused(value != "other"),
            "focus" => {
                // `N,focused|other[,always]`: as the message's menu does.
                let parts: Vec<&str> = value.split(',').collect();
                let index = parts.first().ok_or("focus:N,focused|other[,always]")?.parse()?;
                let focused = parts.get(1) != Some(&"other");
                ui.global::<Mail>().invoke_focus_at(index, focused, parts.get(2) == Some(&"always"));
            }
            "select" => ui.global::<Mail>().invoke_select(value.parse()?, false, false),
            "ctrl-select" => ui.global::<Mail>().invoke_select(value.parse()?, true, false),
            "shift-select" => ui.global::<Mail>().invoke_select(value.parse()?, false, true),
            "person" => ui.global::<People>().invoke_select(value.parse()?),
            "search" => ui.global::<Mail>().invoke_search(value.into()),
            "mode" => ui.global::<Cal>().invoke_set_mode(value.parse()?),
            "next" => ui.global::<Cal>().invoke_next(),
            "previous" => ui.global::<Cal>().invoke_previous(),
            "dark" => ui.global::<Shell>().invoke_dark_changed(true),
            "key" => {
                let mut parts = value.split('+');
                let key = parts.next().unwrap_or_default();
                let rest: Vec<_> = parts.collect();
                ui.global::<Shell>().invoke_key(key.into(), rest.contains(&"ctrl"), rest.contains(&"shift"));
            }
            "hover" => {
                let (x, y) = value.split_once(',').ok_or("hover:X,Y")?;
                let position = LogicalPosition::new(x.parse()?, y.parse()?);
                ui.window().dispatch_event(WindowEvent::PointerMoved { position });
                pump(&queue, Duration::from_millis(800));
            }
            "wheel" => {
                let points: Vec<f32> = value.split(',').map(str::parse).collect::<Result<_, _>>()?;
                let (x, y, delta_x, delta_y) = match points[..] {
                    [x, y, delta_y] => (x, y, 0.0, delta_y),
                    [x, y, delta_x, delta_y] => (x, y, delta_x, delta_y),
                    _ => return Err("wheel:X,Y,DY or wheel:X,Y,DX,DY".into()),
                };
                let position = LogicalPosition::new(x, y);
                ui.window().dispatch_event(WindowEvent::PointerMoved { position });
                ui.window().dispatch_event(WindowEvent::PointerScrolled { position, delta_x, delta_y });
                pump(&queue, Duration::from_millis(300));
            }
            "drag" => {
                let points: Vec<f32> = value.split(',').map(str::parse).collect::<Result<_, _>>()?;
                let [x1, y1, x2, y2] = points[..] else { return Err("drag:X1,Y1,X2,Y2".into()) };
                let button = PointerEventButton::Left;
                let at = |x: f32, y: f32| LogicalPosition::new(x, y);
                ui.window().dispatch_event(WindowEvent::PointerMoved { position: at(x1, y1) });
                ui.window().dispatch_event(WindowEvent::PointerPressed { position: at(x1, y1), button });
                for step in 1..=20 {
                    let t = step as f32 / 20.0;
                    ui.window().dispatch_event(WindowEvent::PointerMoved { position: at(x1 + (x2 - x1) * t, y1 + (y2 - y1) * t) });
                    pump(&queue, Duration::from_millis(15));
                }
                ui.window().dispatch_event(WindowEvent::PointerReleased { position: at(x2, y2), button });
            }
            "click" | "rclick" => {
                let (x, y) = value.split_once(',').ok_or("click:X,Y")?;
                let button = if name == "rclick" { PointerEventButton::Right } else { PointerEventButton::Left };
                click(&ui, x.parse()?, y.parse()?, button);
            }
            other => return Err(format!("unknown step {other}").into()),
        }
        pump(&queue, Duration::from_millis(100));
    }
    Ok(())
}
