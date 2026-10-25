//! Renders the window with sample data to a PNG, without a display.
//!
//!   cargo run -p lookout --example snapshot -- out.png [width height]
//!
//! `SNAPSHOT_PAGE` picks `mail` (the default), `compose`, `calendar`,
//! `month`, `people`, `welcome` or `settings`; `SNAPSHOT_DARK=1` uses the dark
//! theme; `SNAPSHOT_CLICK=x,y` clicks there first (`x,y;x,y` one after the
//! other), `SNAPSHOT_DRAG=x,y,x2` drags from one point to another across and
//! `SNAPSHOT_HOVER=x,y` leaves the pointer there; `SNAPSHOT_UPDATE=1` shows
//! a new version ready to install. `SNAPSHOT_COLLAPSED=1` puts the folder
//! pane and calendars away, `SNAPSHOT_FLYOUT=1` shows the folders over the
//! list, and `SNAPSHOT_FULL=1` gives the reading pane the whole page. On the
//! calendar, `SNAPSHOT_EVENT=1` opens an event and `SNAPSHOT_EDITOR=1` a new
//! one on a read-only calendar.

use std::rc::Rc;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{Color, ComponentHandle, LogicalPosition, ModelRc, PhysicalSize, PlatformError, SharedString, VecModel};
use lookout::*;

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.clone())
    }
}

const BODY: &str = r#"<html><head><style>p{margin:0}</style></head><body>
<p>Hi Ana,</p>
<p>Here are the <b>quarterly numbers</b> we talked about. The full report is in
<a href="https://example.com/report">the shared folder</a>, and the summary is below.</p>
<h2>Summary</h2>
<ul><li>Revenue up <span style="color:#13a10e">12%</span></li><li>Costs down <i>3%</i></li><li>Next review: <u>Friday</u></li></ul>
<p>Let me know if anything looks off — price_list_2024.pdf has the details.</p>
<hr>
<blockquote><p>On Monday, Bo wrote:</p><p>Can you send the numbers before the meeting?</p></blockquote>
<p>Thanks,<br>Carmen</p>
</body></html>"#;

/// A stand-in for a contact's picture: a warm gradient with a lighter disc.
fn photo() -> slint::Image {
    let size = 96u32;
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(size, size);
    for (index, pixel) in buffer.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = ((index as u32 % size) as f32, (index as u32 / size) as f32);
        let head = ((x - 48.0).powi(2) + (y - 38.0).powi(2)).sqrt() < 18.0;
        let body = ((x - 48.0).powi(2) + (y - 100.0).powi(2)).sqrt() < 36.0;
        let shade = (y / size as f32 * 60.0) as u8;
        *pixel = if head || body {
            slint::Rgba8Pixel { r: 250, g: 226, b: 205, a: 255 }
        } else {
            slint::Rgba8Pixel { r: 70 + shade, g: 110 + shade / 2, b: 160, a: 255 }
        };
    }
    lookout::photos::round(&mut buffer);
    slint::Image::from_rgba8(buffer)
}

fn strings(items: &[&str]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(items.iter().map(|s| SharedString::from(*s)).collect::<Vec<_>>()))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: snapshot <out.png> [width height]")?;
    let width = args.next().map_or(Ok(1280), |value| value.parse())?;
    let height = args.next().map_or(Ok(800), |value| value.parse())?;
    let page = std::env::var("SNAPSHOT_PAGE").unwrap_or_else(|_| "mail".into());

    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone())))?;
    let ui = MainWindow::new()?;
    window.set_size(PhysicalSize::new(width, height));
    let dark = std::env::var_os("SNAPSHOT_DARK").is_some();
    ui.global::<Theme>().set_dark(dark);
    set_color_scheme(&ui, dark);

    let shell = ui.global::<Shell>();
    shell.set_status("Up to date at 10:42".into());
    shell.set_version("0.1.0".into());
    if page != "welcome" {
        shell.set_accounts(ModelRc::new(VecModel::from(vec![AccountRow {
            id: "a".into(),
            name: "Carmen Ruiz".into(),
            email: "carmen@outlook.com".into(),
            signature: "Carmen Ruiz\nContoso".into(),
        }])));
        shell.set_account_labels(strings(&["Carmen Ruiz <carmen@outlook.com>"]));
    }
    if page == "settings" {
        shell.set_settings_open(true);
        shell.set_desktop(true);
        shell.set_close_to_tray(true);
        shell.set_notify_new_mail(true);
    }
    let update = ui.global::<AppUpdate>();
    update.set_automatic(true);
    update.set_status("Lookout is up to date.".into());
    if std::env::var_os("SNAPSHOT_UPDATE").is_some() {
        update.set_version("0.2.0".into());
        update.set_ready(true);
        update.set_banner(true);
        update.set_status("Lookout 0.2.0 is ready to install.".into());
    }

    let icons = ui.global::<Icons>();
    let folder = |name: &str, icon: SharedString, depth: i32, unread: i32| FolderRow {
        account: "a".into(),
        id: name.into(),
        name: name.into(),
        depth,
        unread,
        icon,
        ..FolderRow::default()
    };
    let mail = ui.global::<Mail>();
    mail.set_folders(ModelRc::new(VecModel::from(vec![
        folder("Inbox", icons.get_inbox(), 0, 3),
        folder("Receipts", icons.get_folder(), 1, 0),
        folder("Drafts", icons.get_edit(), 0, 1),
        folder("Sent Items", icons.get_send(), 0, 0),
        folder("Archive", icons.get_archive(), 0, 0),
        folder("Deleted Items", icons.get_delete(), 0, 0),
        folder("Junk Email", icons.get_junk(), 0, 0),
        FolderRow { account: "a".into(), divider: true, ..FolderRow::default() },
        folder("Projects", icons.get_folder(), 0, 12),
        folder("Receipts 2025", icons.get_folder(), 0, 0),
        folder("Travel", icons.get_folder(), 0, 2),
        folder("Utilities", icons.get_folder(), 0, 0),
        folder("Very long folder name that elides", icons.get_folder(), 0, 128),
    ])));
    mail.set_strip(ModelRc::new(VecModel::from(vec![
        folder("Inbox", icons.get_inbox(), 0, 3),
        folder("Drafts", icons.get_edit(), 0, 1),
        folder("Sent Items", icons.get_send(), 0, 0),
        folder("Archive", icons.get_archive(), 0, 0),
        folder("Deleted Items", icons.get_delete(), 0, 0),
        folder("Junk Email", icons.get_junk(), 0, 0),
    ])));
    let panes = ui.global::<Panes>();
    let flag = |name: &str| std::env::var_os(name).is_some();
    panes.set_folders_collapsed(flag("SNAPSHOT_COLLAPSED"));
    panes.set_calendars_collapsed(flag("SNAPSHOT_COLLAPSED"));
    panes.set_folders_flyout(flag("SNAPSHOT_FLYOUT"));
    panes.set_reading_only(flag("SNAPSHOT_FULL"));
    mail.set_selected_account("a".into());
    mail.set_selected_folder("Inbox".into());
    mail.set_list_title("Inbox".into());
    let row = |sender: &str, subject: &str, preview: &str, date: &str, unread: bool| MessageRow {
        account: "a".into(),
        id: subject.into(),
        sender: sender.into(),
        subject: subject.into(),
        preview: preview.into(),
        date: date.into(),
        unread,
        initials: sender.split_whitespace().filter_map(|word| word.chars().next()).take(2).collect::<String>().into(),
        hue: sender.len() as i32 % 10,
        ..MessageRow::default()
    };
    let mut rows = vec![
        row("Carmen Ruiz", "Quarterly numbers", "Here are the quarterly numbers we talked about. The full report is in the shared folder", "10:31", true),
        row("GitHub", "[lookout] Build passed", "All checks have passed on master", "09:12", true),
        row("Bo Lindqvist", "Lunch on Friday?", "There is a new place near the office that does great ramen", "Thu 18:02", false),
        row("Microsoft account team", "New sign-in to your account", "We noticed a new sign-in to your Microsoft account", "Thu 07:45", true),
        row("Ana Pérez", "Re: Trip to Lisbon", "Sounds good! I booked the flights for the 14th", "Wed 21/10", false),
        row("Newsletter", "This week in Rust", "Hello and welcome to another issue of This Week in Rust", "Mon 19/10", false),
    ];
    rows[1].attachment = true;
    rows[2].flagged = true;
    rows[4].picture = photo();
    rows[3].important = true;
    mail.set_messages(ModelRc::new(VecModel::from(rows)));

    if page == "mail" || page == "compose" {
        mail.set_selected(0);
        let rendered = lookout_core::html::render(BODY);
        mail.set_blocks(ModelRc::new(VecModel::from(lookout::body::blocks(&rendered, dark))));
        mail.set_attachments(ModelRc::new(VecModel::from(vec![AttachmentRow {
            id: "1".into(),
            name: "Q3-report.pdf".into(),
            size: "412 KB".into(),
        }])));
        mail.set_reader(Reader {
            open: true,
            account: "a".into(),
            id: "1".into(),
            subject: "Quarterly numbers".into(),
            sender: "Carmen Ruiz".into(),
            sender_address: "carmen@contoso.com".into(),
            initials: "CR".into(),
            hue: 3,
            to: "Ana Pérez; Bo Lindqvist".into(),
            date: "Fri 23/10/2026 10:31".into(),
            remote_images: true,
            plain: rendered.text.into(),
            ..Reader::default()
        });
    }
    if page == "compose" {
        mail.set_compose(Compose {
            open: true,
            to: "Ana Pérez <ana@example.com>; ".into(),
            subject: "RE: Quarterly numbers".into(),
            body: "Thanks Carmen, **looks great**.\n\n- one\n- two\n\nAna".into(),
            quote: "Here are the quarterly numbers we talked about.".into(),
            kind: 1,
            ..Compose::default()
        });
        let suggestion = |name: &str, address: &str, initials: &str, hue: i32| Suggestion {
            name: name.into(),
            address: address.into(),
            initials: initials.into(),
            hue,
            picture: if hue == 4 { photo() } else { Default::default() },
            label: format!("{name} <{address}>").into(),
        };
        mail.set_suggestions(ModelRc::new(VecModel::from(vec![
            suggestion("Bo Lindqvist", "bo@example.com", "BL", 1),
            suggestion("Borja Gil", "borja@example.com", "BG", 4),
            suggestion("", "boris@example.org", "B", 6),
        ])));
        mail.set_suggestion_field(0);
        mail.on_suggestion_picked(|_, current, picked| {
            let kept = current.rfind(';').map(|at| &current[..=at]).unwrap_or_default();
            format!("{kept} {picked}; ").trim_start().into()
        });
    }

    if page == "calendar" || page == "month" {
        shell.set_page(Page::Calendar);
        let cal = ui.global::<Cal>();
        cal.set_title(if page == "month" { "October 2026" } else { "19 – 25 October 2026" }.into());
        cal.set_mode(if page == "month" { 3 } else { 2 });
        let blue = Color::from_rgb_u8(0x0f, 0x6c, 0xbd);
        let green = Color::from_rgb_u8(0x13, 0xa1, 0x0e);
        cal.set_calendars(ModelRc::new(VecModel::from(vec![
            CalendarRow { account: "a".into(), name: "carmen@outlook.com".into(), header: true, ..Default::default() },
            CalendarRow { account: "a".into(), id: "c".into(), name: "Calendar".into(), color: blue, shown: true, header: false },
            CalendarRow { account: "a".into(), id: "b".into(), name: "Birthdays".into(), color: green, shown: false, header: false },
        ])));
        let days: Vec<DayColumn> = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
            .iter()
            .enumerate()
            .map(|(i, d)| DayColumn { weekday: (*d).into(), day: (19 + i).to_string().into(), today: i == 4 })
            .collect();
        cal.set_days(ModelRc::new(VecModel::from(days)));
        let event = |day: i32, start: f32, hours: f32, title: &str, column: i32, columns: i32| EventBox {
            account: "a".into(),
            id: title.into(),
            day,
            top: start / 24.0,
            height: hours / 24.0,
            column,
            columns,
            title: title.into(),
            time: format!("{:02}:00 – {:02}:00", start as i32, (start + hours) as i32).into(),
            location: if title == "Standup" { "Teams".into() } else { SharedString::new() },
            color: blue,
            ..Default::default()
        };
        cal.set_events(ModelRc::new(VecModel::from(vec![
            event(0, 9.0, 0.5, "Standup", 0, 1),
            event(1, 9.0, 0.5, "Standup", 0, 1),
            event(2, 10.0, 2.0, "Design review", 0, 2),
            event(2, 11.0, 1.0, "Lunch with Bo", 1, 2),
            event(4, 14.0, 1.5, "Quarterly numbers", 0, 1),
        ])));
        cal.set_all_day(ModelRc::new(VecModel::from(vec![AllDayBox {
            account: "a".into(),
            id: "trip".into(),
            first_day: 5,
            days: 2,
            row: 0,
            title: "Trip to Lisbon".into(),
            color: green,
        }])));
        cal.set_all_day_rows(1);
        cal.set_now_day(4);
        cal.set_now_fraction(10.7 / 24.0);
        let cells: Vec<MonthCell> = (0..42)
            .map(|i| MonthCell {
                day: (((i + 28) % 31) + 1).to_string().into(),
                in_month: (3..34).contains(&i),
                today: i == 25,
                events: if i % 5 == 0 {
                    ModelRc::new(VecModel::from(vec![
                        MonthEvent { text: "09:00 Standup".into(), color: blue, all_day: false },
                        MonthEvent { text: "Trip to Lisbon".into(), color: green, all_day: true },
                    ]))
                } else {
                    ModelRc::default()
                },
                more: if i == 10 { 2 } else { 0 },
            })
            .collect();
        cal.set_month(ModelRc::new(VecModel::from(cells)));
        if std::env::var_os("SNAPSHOT_EVENT").is_some() {
            cal.set_detail(EventDetail {
                open: true,
                subject: "Design review".into(),
                when: "Wed 21/10/2026 10:00 – 12:00".into(),
                location: "Room 4".into(),
                organizer: "Organizer: Bo Lindqvist <bo@example.com>".into(),
                attendees: "Attendees: Carmen Ruiz ✓, Ana Pérez ?".into(),
                response: "You haven't responded".into(),
                can_respond: true,
                join_url: "https://teams.example/j".into(),
                calendar: "Calendar".into(),
                color: blue,
                ..Default::default()
            });
        }
        if std::env::var_os("SNAPSHOT_EDITOR").is_some() {
            cal.set_calendar_labels(strings(&["Calendar", "Birthdays (read-only)"]));
            cal.set_calendar_writable(ModelRc::new(VecModel::from(vec![true, false])));
            cal.set_editor(EventEditor {
                open: true,
                calendar_index: 1,
                subject: "Design review".into(),
                start_date: "2026-10-21".into(),
                start_time: "10:00".into(),
                end_date: "2026-10-21".into(),
                end_time: "12:00".into(),
                reminder_index: 2,
                ..Default::default()
            });
        }
    }

    if page == "people" {
        shell.set_page(Page::People);
        let people = ui.global::<People>();
        let contact = |name: &str, email: &str, initials: &str| ContactRow {
            account: "a".into(),
            id: name.into(),
            name: name.into(),
            email: email.into(),
            initials: initials.into(),
            hue: email.len() as i32 % 10,
            picture: if name.starts_with("Bo") { photo() } else { Default::default() },
            detail: email.into(),
        };
        people.set_contacts(ModelRc::new(VecModel::from(vec![
            contact("Ana Pérez", "ana@example.com", "AP"),
            contact("Bo Lindqvist", "bo@example.com", "BL"),
            contact("Carmen Ruiz", "carmen@contoso.com", "CR"),
        ])));
        people.set_selected(0);
        people.set_contact(ContactView {
            shown: true,
            name: "Ana Pérez".into(),
            initials: "AP".into(),
            hue: 5,
            picture: photo(),
            lines: strings(&["Product manager, Contoso", "Mobile: +34 600 000 000"]),
            emails: strings(&["ana@example.com"]),
            notes: "Met at RustConf.".into(),
            ..Default::default()
        });
    }

    ui.show()?;
    slint::platform::update_timers_and_animations();
    let clicks = std::env::var("SNAPSHOT_CLICK").unwrap_or_default();
    for (x, y) in clicks.split(';').filter_map(|point| point.split_once(',')) {
        let position = LogicalPosition::new(x.trim().parse()?, y.trim().parse()?);
        let button = PointerEventButton::Left;
        for event in [
            WindowEvent::PointerMoved { position },
            WindowEvent::PointerPressed { position, button },
            WindowEvent::PointerReleased { position, button },
        ] {
            ui.window().dispatch_event(event);
        }
        slint::platform::update_timers_and_animations();
    }
    if let Ok(points) = std::env::var("SNAPSHOT_DRAG") {
        let numbers: Vec<f32> = points.split(',').map(|n| n.trim().parse()).collect::<Result<_, _>>()?;
        let [x, y, to] = numbers[..] else { return Err("SNAPSHOT_DRAG=x,y,x2".into()) };
        let button = PointerEventButton::Left;
        let at = |x: f32| LogicalPosition::new(x, y);
        ui.window().dispatch_event(WindowEvent::PointerMoved { position: at(x) });
        ui.window().dispatch_event(WindowEvent::PointerPressed { position: at(x), button });
        for step in 1..=10 {
            let position = at(x + (to - x) * step as f32 / 10.0);
            ui.window().dispatch_event(WindowEvent::PointerMoved { position });
            slint::platform::update_timers_and_animations();
        }
        ui.window().dispatch_event(WindowEvent::PointerReleased { position: at(to), button });
        slint::platform::update_timers_and_animations();
    }
    if let Some((x, y)) = std::env::var("SNAPSHOT_HOVER").ok().as_deref().and_then(|p| p.split_once(',')) {
        let position = LogicalPosition::new(x.trim().parse()?, y.trim().parse()?);
        ui.window().dispatch_event(WindowEvent::PointerMoved { position });
        slint::platform::update_timers_and_animations();
    }
    let snapshot = ui.window().take_snapshot()?;
    image::save_buffer(&path, snapshot.as_bytes(), snapshot.width(), snapshot.height(), image::ColorType::Rgba8)?;
    println!("wrote {path} ({}x{})", snapshot.width(), snapshot.height());
    Ok(())
}
