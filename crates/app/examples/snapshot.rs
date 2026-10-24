//! Renders the window with sample data to a PNG, without a display.
//!
//!   cargo run -p lookout --example snapshot -- out.png [width height]
//!
//! `SNAPSHOT_PAGE` picks `mail` (the default), `compose`, `calendar`,
//! `month`, `people`, `welcome` or `settings`; `SNAPSHOT_DARK=1` uses the dark
//! theme; `SNAPSHOT_CLICK=x,y` clicks there first.

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
        mail.set_suggestions(strings(&["Bo Lindqvist <bo@example.com>", "Borja Gil <borja@example.com>"]));
        mail.set_suggestion_field(0);
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
            lines: strings(&["Product manager, Contoso", "Mobile: +34 600 000 000"]),
            emails: strings(&["ana@example.com"]),
            notes: "Met at RustConf.".into(),
            ..Default::default()
        });
    }

    ui.show()?;
    slint::platform::update_timers_and_animations();
    if let Some((x, y)) = std::env::var("SNAPSHOT_CLICK").ok().as_deref().and_then(|p| p.split_once(',')) {
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
    let snapshot = ui.window().take_snapshot()?;
    image::save_buffer(&path, snapshot.as_bytes(), snapshot.width(), snapshot.height(), image::ColorType::Rgba8)?;
    println!("wrote {path} ({}x{})", snapshot.width(), snapshot.height());
    Ok(())
}
