//! The calendar page's behaviour.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use chrono::{Datelike, Days, Local, NaiveDate, NaiveTime, TimeZone, Timelike};
use lookout_core::calendar::{Answer, EventDraft};
use lookout_core::model::{Calendar, Event, Response};
use lookout_core::{html, when};
use slint::{Color, ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::app::App;
use crate::engine::Request;
use crate::{AllDayBox, Cal, CalendarRow, DayColumn, EventBox, EventDetail, EventEditor, MainWindow, MiniDay, MonthCell, MonthEvent, format};

const PALETTE: &[u32] = &[0x0f6cbd, 0xc239b3, 0x13a10e, 0xca5010, 0x8764b8, 0x038387, 0x986f0b, 0xd13438];
const MONTH_LINES: usize = 3;
const REMINDERS: &[Option<u32>] = &[None, Some(5), Some(15), Some(30), Some(60), Some(1440)];
const SHOW_AS: &[&str] = &["busy", "tentative", "free", "oof"];
const DEFAULT_REMINDER: i32 = 2;

pub struct CalState {
    mode: Cell<i32>,
    anchor: Cell<NaiveDate>,
    /// The first of the month the sidebar shows.
    mini_month: Cell<NaiveDate>,
    /// The anchor and mode last shown, so the sidebar's month only follows
    /// the view when the view moved.
    last_view: Cell<(NaiveDate, i32)>,
    days: RefCell<Vec<NaiveDate>>,
    colors: RefCell<HashMap<(String, String), Color>>,
    names: RefCell<HashMap<(String, String), String>>,
    /// The calendars as the editor lists them, and whether events can be
    /// added to each.
    pickable: RefCell<Vec<(String, String, bool)>>,
    open: RefCell<Option<(String, String)>>,
    /// The description being edited, as text and as it was, so an
    /// unchanged one keeps its formatting.
    original_body: RefCell<Option<(String, String)>>,
    clock: Timer,
}

impl CalState {
    pub fn new() -> Self {
        Self {
            mode: Cell::new(2),
            anchor: Cell::new(Local::now().date_naive()),
            mini_month: Cell::new(Local::now().date_naive().with_day(1).expect("the first exists")),
            last_view: Cell::new((NaiveDate::MIN, -1)),
            days: RefCell::new(Vec::new()),
            colors: RefCell::new(HashMap::new()),
            names: RefCell::new(HashMap::new()),
            pickable: RefCell::new(Vec::new()),
            open: RefCell::new(None),
            original_body: RefCell::new(None),
            clock: Timer::default(),
        }
    }
}

macro_rules! with_app {
    ($app:expr, |$name:ident $(, $arg:ident)*| $body:expr) => {{
        let weak = Rc::downgrade($app);
        move |$($arg),*| {
            if let Some($name) = weak.upgrade() {
                $body
            }
        }
    }};
}

pub fn wire(app: &Rc<App>, ui: &MainWindow) {
    let cal = ui.global::<Cal>();
    cal.on_today(with_app!(app, |app| {
        app.cal.anchor.set(Local::now().date_naive());
        refresh(&app);
    }));
    cal.on_previous(with_app!(app, |app| step(&app, -1)));
    cal.on_mini_step(with_app!(app, |app, direction| {
        let month = app.cal.mini_month.get();
        let moved = if direction > 0 {
            month.checked_add_months(chrono::Months::new(1))
        } else {
            month.checked_sub_months(chrono::Months::new(1))
        };
        app.cal.mini_month.set(moved.unwrap_or(month));
        show_mini(&app);
    }));
    cal.on_mini_pick(with_app!(app, |app, index| {
        let day = shift_days(monday_of(app.cal.mini_month.get()), i64::from(index.max(0)));
        app.cal.anchor.set(day);
        refresh(&app);
    }));
    cal.on_next(with_app!(app, |app| step(&app, 1)));
    cal.on_set_mode(with_app!(app, |app, mode| {
        app.cal.mode.set(mode);
        refresh(&app);
    }));
    cal.on_open_event(with_app!(app, |app, account, id| open_event(&app, &account, &id)));
    cal.on_close_event(with_app!(app, |app| {
        *app.cal.open.borrow_mut() = None;
        if let Some(ui) = app.ui() {
            ui.global::<Cal>().set_detail(EventDetail::default());
        }
    }));
    cal.on_new_event(with_app!(app, |app| {
        let start = next_half_hour();
        new_event(&app, start, start + 1800, false);
    }));
    cal.on_new_event_at(with_app!(app, |app, day, slot| {
        let Some(date) = app.cal.days.borrow().get(day.max(0) as usize).copied() else { return };
        let start = when::local_midnight(date) + i64::from(slot.clamp(0, 47)) * 1800;
        new_event(&app, start, start + 1800, false);
    }));
    cal.on_open_day(with_app!(app, |app, index| {
        let day = if app.cal.mode.get() == 3 {
            month_start(app.cal.anchor.get()).checked_add_days(Days::new(index.max(0) as u64))
        } else {
            app.cal.days.borrow().get(index.max(0) as usize).copied()
        };
        if let Some(day) = day {
            app.cal.anchor.set(day);
            app.cal.mode.set(0);
            refresh(&app);
        }
    }));
    cal.on_toggle_calendar(with_app!(app, |app, account, id, shown| {
        if let Err(error) = app.store.set_calendar_shown(&account, &id, shown) {
            app.status(&error.to_string());
        }
        refresh(&app);
    }));
    cal.on_respond(with_app!(app, |app, answer| {
        let Some((account, id)) = app.cal.open.borrow_mut().take() else { return };
        let answer = match answer {
            0 => Answer::Accept,
            1 => Answer::Tentative,
            _ => Answer::Decline,
        };
        app.engine.send(Request::Respond { account, id, answer });
        if let Some(ui) = app.ui() {
            ui.global::<Cal>().set_detail(EventDetail::default());
        }
    }));
    cal.on_edit_event(with_app!(app, |app| edit_event(&app)));
    cal.on_delete_event(with_app!(app, |app| {
        let Some((account, id)) = app.cal.open.borrow_mut().take() else { return };
        let cancel = app
            .store
            .event(&account, &id)
            .ok()
            .flatten()
            .is_some_and(|event| event.response == Response::Organizer && !event.attendees.is_empty());
        app.engine.send(Request::DeleteEvent { account, id, cancel });
        if let Some(ui) = app.ui() {
            ui.global::<Cal>().set_detail(EventDetail::default());
        }
    }));
    cal.on_join(with_app!(app, |app| {
        if let Some(ui) = app.ui() {
            let url = ui.global::<Cal>().get_detail().join_url;
            if let Err(error) = open::that_detached(url.as_str()) {
                app.toast(&format!("Can't open the meeting: {error}"));
            }
        }
    }));
    cal.on_open_in_browser(with_app!(app, |app| {
        if let Some(ui) = app.ui() {
            let url = ui.global::<Cal>().get_detail().web_link;
            _ = open::that_detached(url.as_str());
        }
    }));
    cal.on_save_event(with_app!(app, |app, editor| save(&app, editor)));
    cal.on_cancel_editor(with_app!(app, |app| {
        if let Some(ui) = app.ui() {
            ui.global::<Cal>().set_editor(EventEditor::default());
        }
    }));

    // The red line for now moves on.
    let weak = Rc::downgrade(app);
    app.cal.clock.start(TimerMode::Repeated, Duration::from_secs(60), move || {
        if let Some(app) = weak.upgrade() {
            update_now(&app);
        }
    });
}

fn step(app: &Rc<App>, direction: i64) {
    let anchor = app.cal.anchor.get();
    let moved = match app.cal.mode.get() {
        0 => shift_days(anchor, direction),
        1 | 2 => shift_days(anchor, 7 * direction),
        _ => {
            let month = anchor.with_day(1).expect("the first exists");
            if direction > 0 { month.checked_add_months(chrono::Months::new(1)) } else { month.checked_sub_months(chrono::Months::new(1)) }
                .unwrap_or(month)
        }
    };
    app.cal.anchor.set(moved);
    refresh(app);
}

fn shift_days(date: NaiveDate, days: i64) -> NaiveDate {
    if days >= 0 { date.checked_add_days(Days::new(days as u64)) } else { date.checked_sub_days(Days::new((-days) as u64)) }
        .unwrap_or(date)
}

fn monday_of(date: NaiveDate) -> NaiveDate {
    shift_days(date, -i64::from(date.weekday().num_days_from_monday()))
}

fn month_start(anchor: NaiveDate) -> NaiveDate {
    monday_of(anchor.with_day(1).expect("the first exists"))
}

fn visible_days(mode: i32, anchor: NaiveDate) -> Vec<NaiveDate> {
    let (first, count) = match mode {
        0 => (anchor, 1),
        1 => (monday_of(anchor), 5),
        2 => (monday_of(anchor), 7),
        _ => (month_start(anchor), 42),
    };
    (0..count).map(|offset| shift_days(first, offset)).collect()
}

fn color_of(hex: &str, index: usize) -> Color {
    let parsed = hex.strip_prefix('#').and_then(|hex| u32::from_str_radix(hex, 16).ok()).filter(|_| hex.len() == 7);
    let rgb = parsed.unwrap_or(PALETTE[index % PALETTE.len()]);
    Color::from_rgb_u8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

fn load_calendars(app: &App, ui: &MainWindow) {
    let calendars = app.store.calendars().unwrap_or_default();
    let several = app.accounts.borrow().len() > 1;
    let mut rows = Vec::new();
    let mut colors = HashMap::new();
    let mut names = HashMap::new();
    let mut pickable = Vec::new();
    let mut labels: Vec<SharedString> = Vec::new();
    let mut writable = Vec::new();
    let mut last_account = String::new();
    for (index, (account, calendar, shown)) in calendars.iter().enumerate() {
        if *account != last_account {
            last_account = account.clone();
            let email = app.accounts.borrow().iter().find(|a| &a.id == account).map(|a| a.email.clone()).unwrap_or_default();
            rows.push(CalendarRow { account: account.as_str().into(), name: email.into(), header: true, ..Default::default() });
        }
        let color = color_of(&calendar.color, index);
        colors.insert((account.clone(), calendar.id.clone()), color);
        names.insert((account.clone(), calendar.id.clone()), calendar.name.clone());
        rows.push(CalendarRow {
            account: account.as_str().into(),
            id: calendar.id.as_str().into(),
            name: calendar.name.as_str().into(),
            color,
            shown: *shown,
            header: false,
        });
        pickable.push((account.clone(), calendar.id.clone(), calendar.can_edit));
        let name = label(app, account, calendar, several);
        labels.push(if calendar.can_edit { name } else { format!("{name} (read-only)") }.into());
        writable.push(calendar.can_edit);
    }
    *app.cal.colors.borrow_mut() = colors;
    *app.cal.names.borrow_mut() = names;
    *app.cal.pickable.borrow_mut() = pickable;
    let cal = ui.global::<Cal>();
    cal.set_calendars(ModelRc::new(VecModel::from(rows)));
    cal.set_calendar_labels(ModelRc::new(VecModel::from(labels)));
    cal.set_calendar_writable(ModelRc::new(VecModel::from(writable)));
}

fn label(app: &App, account: &str, calendar: &Calendar, several: bool) -> String {
    if several {
        let email = app.accounts.borrow().iter().find(|a| a.id == account).map(|a| a.email.clone()).unwrap_or_default();
        format!("{} ({email})", calendar.name)
    } else {
        calendar.name.clone()
    }
}

/// Shows the range around the anchor date, from the cache, and asks the
/// engine for it if it is outside what is kept.
pub fn refresh(app: &Rc<App>) {
    let Some(ui) = app.ui() else { return };
    load_calendars(app, &ui);
    let mode = app.cal.mode.get();
    let anchor = app.cal.anchor.get();
    let days = visible_days(mode, anchor);
    let start = when::local_midnight(days[0]);
    let end = when::local_midnight(shift_days(*days.last().expect("days are shown"), 1));
    let events = app.store.events_between(start, end).unwrap_or_default();
    let cal = ui.global::<Cal>();
    cal.set_mode(mode);
    cal.set_title(title(mode, anchor, &days).into());
    let today = Local::now().date_naive();
    if mode == 3 {
        let colors = app.cal.colors.borrow();
        cal.set_month(ModelRc::new(VecModel::from(month_cells(&days, anchor, today, &events, &colors))));
    } else {
        let columns: Vec<DayColumn> = days
            .iter()
            .map(|day| DayColumn {
                weekday: day.format("%a").to_string().into(),
                day: day.day().to_string().into(),
                today: *day == today,
            })
            .collect();
        let colors = app.cal.colors.borrow();
        let (timed, all_day, rows) = layout(&days, &events, &colors);
        cal.set_days(ModelRc::new(VecModel::from(columns)));
        cal.set_events(ModelRc::new(VecModel::from(timed)));
        cal.set_all_day(ModelRc::new(VecModel::from(all_day)));
        cal.set_all_day_rows(rows);
    }
    // The sidebar's month follows the view when it moves away from it,
    // but not when the view is only redrawn.
    if app.cal.last_view.replace((anchor, mode)) != (anchor, mode) {
        let mini = app.cal.mini_month.get();
        if !days.iter().any(|day| day.year() == mini.year() && day.month() == mini.month()) || mode == 3 {
            app.cal.mini_month.set(anchor.with_day(1).expect("the first exists"));
        }
    }
    *app.cal.days.borrow_mut() = days;
    show_mini(app);
    update_now(app);
    app.engine.send(Request::CalendarRange { start, end });
}

/// The sidebar's month: six weeks from the Monday before its first, the
/// days the view shows marked.
fn show_mini(app: &App) {
    let Some(ui) = app.ui() else { return };
    let month = app.cal.mini_month.get();
    let today = Local::now().date_naive();
    let days = app.cal.days.borrow();
    let mode = app.cal.mode.get();
    let first = monday_of(month);
    let cells: Vec<MiniDay> = (0..42)
        .map(|offset| {
            let day = shift_days(first, offset);
            MiniDay {
                day: day.day().to_string().into(),
                in_month: day.month() == month.month(),
                today: day == today,
                // A month view shows its own month, not six weeks.
                shown: if mode == 3 { day.month() == app.cal.anchor.get().month() && day.year() == app.cal.anchor.get().year() } else { days.contains(&day) },
            }
        })
        .collect();
    let cal = ui.global::<Cal>();
    cal.set_mini_title(month.format("%B %Y").to_string().into());
    cal.set_mini_days(ModelRc::new(VecModel::from(cells)));
}

fn update_now(app: &App) {
    let Some(ui) = app.ui() else { return };
    let now = Local::now();
    let cal = ui.global::<Cal>();
    let index = app.cal.days.borrow().iter().position(|day| *day == now.date_naive());
    cal.set_now_day(index.map(|i| i as i32).unwrap_or(-1));
    cal.set_now_fraction((now.hour() * 60 + now.minute()) as f32 / 1440.0);
}

fn title(mode: i32, anchor: NaiveDate, days: &[NaiveDate]) -> String {
    match mode {
        0 => anchor.format("%A, %-d %B %Y").to_string(),
        3 => anchor.format("%B %Y").to_string(),
        _ => {
            let (first, last) = (days[0], days[days.len() - 1]);
            if first.month() == last.month() {
                format!("{} – {}", first.format("%-d"), last.format("%-d %B %Y"))
            } else if first.year() == last.year() {
                format!("{} – {}", first.format("%-d %b"), last.format("%-d %b %Y"))
            } else {
                format!("{} – {}", first.format("%-d %b %Y"), last.format("%-d %b %Y"))
            }
        }
    }
}

/// The local dates an event covers, the last one included.
fn event_dates(event: &Event) -> (NaiveDate, NaiveDate) {
    if event.all_day {
        let first = when::floating_date(event.start);
        let last = shift_days(when::floating_date(event.end), -1).max(first);
        (first, last)
    } else {
        let first = when::local(event.start).date_naive();
        // An event ending at midnight doesn't reach into the next day.
        let last = when::local(event.end - 1).date_naive().max(first);
        (first, last)
    }
}

fn in_strip(event: &Event) -> bool {
    event.all_day || event.end - event.start >= 86400
}

fn layout(
    days: &[NaiveDate],
    events: &[Event],
    colors: &HashMap<(String, String), Color>,
) -> (Vec<EventBox>, Vec<AllDayBox>, i32) {
    let color = |event: &Event| {
        colors.get(&(event.account.clone(), event.calendar_id.clone())).copied().unwrap_or_else(|| color_of("", 0))
    };
    // All-day and multi-day events, in rows without overlaps.
    let mut strip = Vec::new();
    let mut rows_used: Vec<Vec<(usize, usize)>> = Vec::new();
    for event in events.iter().filter(|event| in_strip(event)) {
        let (first, last) = event_dates(event);
        let Some(start) = days.iter().position(|day| *day >= first) else { continue };
        let end = days.iter().rposition(|day| *day <= last).unwrap_or(start);
        if end < start {
            continue;
        }
        let row = rows_used
            .iter()
            .position(|row| row.iter().all(|&(a, b)| end < a || start > b))
            .unwrap_or_else(|| {
                rows_used.push(Vec::new());
                rows_used.len() - 1
            });
        rows_used[row].push((start, end));
        strip.push(AllDayBox {
            account: event.account.as_str().into(),
            id: event.id.as_str().into(),
            first_day: start as i32,
            days: (end - start + 1) as i32,
            row: row as i32,
            title: event.subject.as_str().into(),
            color: color(event),
        });
    }

    let mut boxes = Vec::new();
    for (index, day) in days.iter().enumerate() {
        let day_start = when::local_midnight(*day);
        let day_end = when::local_midnight(shift_days(*day, 1));
        let length = (day_end - day_start).max(1) as f32;
        let mut today: Vec<(i64, i64, &Event)> = events
            .iter()
            .filter(|event| !in_strip(event) && event.start < day_end && event.end > day_start)
            .map(|event| (event.start.max(day_start), event.end.min(day_end).max(event.start.max(day_start) + 900), event))
            .collect();
        today.sort_by_key(|(start, end, _)| (*start, -*end));
        // Overlapping events share the day's width.
        let mut cluster: Vec<(i64, i64, &Event, usize)> = Vec::new();
        let mut cluster_end = i64::MIN;
        let flush = |cluster: &mut Vec<(i64, i64, &Event, usize)>, boxes: &mut Vec<EventBox>| {
            let columns = cluster.iter().map(|(.., column)| column + 1).max().unwrap_or(1);
            for (start, end, event, column) in cluster.drain(..) {
                boxes.push(EventBox {
                    account: event.account.as_str().into(),
                    id: event.id.as_str().into(),
                    day: index as i32,
                    top: (start - day_start) as f32 / length,
                    height: (end - start) as f32 / length,
                    column: column as i32,
                    columns: columns as i32,
                    title: event.subject.as_str().into(),
                    time: format!("{} – {}", when::clock(event.start), when::clock(event.end)).into(),
                    location: event.location.as_str().into(),
                    color: color(event),
                    tentative: matches!(event.response, Response::Tentative | Response::NotResponded)
                        || event.show_as == "tentative",
                    declined: event.response == Response::Declined,
                    cancelled: event.is_cancelled,
                });
            }
        };
        for (start, end, event) in today {
            if start >= cluster_end && !cluster.is_empty() {
                flush(&mut cluster, &mut boxes);
            }
            let mut ends: Vec<i64> = Vec::new();
            for (_, other_end, _, column) in &cluster {
                if ends.len() <= *column {
                    ends.resize(column + 1, i64::MIN);
                }
                ends[*column] = ends[*column].max(*other_end);
            }
            let column = ends.iter().position(|&taken| taken <= start).unwrap_or(ends.len());
            cluster.push((start, end, event, column));
            cluster_end = cluster_end.max(end);
        }
        flush(&mut cluster, &mut boxes);
    }
    (boxes, strip, rows_used.len() as i32)
}

fn month_cells(
    days: &[NaiveDate],
    anchor: NaiveDate,
    today: NaiveDate,
    events: &[Event],
    colors: &HashMap<(String, String), Color>,
) -> Vec<MonthCell> {
    days.iter()
        .map(|day| {
            let on_day: Vec<&Event> = events
                .iter()
                .filter(|event| {
                    let (first, last) = event_dates(event);
                    first <= *day && *day <= last
                })
                .collect();
            let lines: Vec<MonthEvent> = on_day
                .iter()
                .take(MONTH_LINES)
                .map(|event| MonthEvent {
                    text: if in_strip(event) {
                        event.subject.as_str().into()
                    } else {
                        format!("{} {}", when::clock(event.start), event.subject).into()
                    },
                    color: colors
                        .get(&(event.account.clone(), event.calendar_id.clone()))
                        .copied()
                        .unwrap_or_else(|| color_of("", 0)),
                    all_day: in_strip(event),
                })
                .collect();
            MonthCell {
                day: if day.day() == 1 { day.format("%-d %b").to_string() } else { day.day().to_string() }.into(),
                in_month: day.month() == anchor.month(),
                today: *day == today,
                events: ModelRc::new(VecModel::from(lines)),
                more: on_day.len().saturating_sub(MONTH_LINES) as i32,
            }
        })
        .collect()
}

fn response_label(response: Response) -> &'static str {
    match response {
        Response::Accepted => "You accepted",
        Response::Tentative => "You tentatively accepted",
        Response::Declined => "You declined",
        Response::NotResponded => "You haven't responded",
        Response::Organizer | Response::None => "",
    }
}

fn when_text(event: &Event) -> String {
    if event.all_day {
        let (first, last) = event_dates(event);
        if first == last {
            format!("{} (all day)", first.format("%a %d/%m/%Y"))
        } else {
            format!("{} – {} (all day)", first.format("%a %d/%m/%Y"), last.format("%a %d/%m/%Y"))
        }
    } else {
        let start = when::local(event.start);
        let end = when::local(event.end);
        if start.date_naive() == end.date_naive() {
            format!("{} {} – {}", start.format("%a %d/%m/%Y"), start.format("%H:%M"), end.format("%H:%M"))
        } else {
            format!("{} – {}", start.format("%a %d/%m/%Y %H:%M"), end.format("%a %d/%m/%Y %H:%M"))
        }
    }
}

fn open_event(app: &Rc<App>, account: &str, id: &str) {
    let Some(ui) = app.ui() else { return };
    let Ok(Some(event)) = app.store.event(account, id) else { return };
    *app.cal.open.borrow_mut() = Some((account.into(), id.into()));
    let key = (event.account.clone(), event.calendar_id.clone());
    let attendees: Vec<String> = event
        .attendees
        .iter()
        .map(|attendee| {
            let mark = match attendee.response {
                Response::Accepted => " ✓",
                Response::Declined => " ✗",
                Response::Tentative => " ?",
                _ => "",
            };
            format!("{}{mark}", attendee.address.display())
        })
        .collect();
    ui.global::<Cal>().set_detail(EventDetail {
        open: true,
        account: account.into(),
        id: id.into(),
        subject: event.subject.as_str().into(),
        when: when_text(&event).into(),
        location: event.location.as_str().into(),
        organizer: if event.organizer.address.is_empty() || event.response == Response::Organizer {
            SharedString::new()
        } else {
            format!("Organizer: {}", format::address(&event.organizer)).into()
        },
        attendees: if attendees.is_empty() { SharedString::new() } else { format!("Attendees: {}", attendees.join(", ")).into() },
        response: if event.is_cancelled { "Cancelled".into() } else { response_label(event.response).into() },
        can_respond: event.response != Response::Organizer && !event.is_cancelled,
        is_organizer: event.response == Response::Organizer,
        join_url: event.join_url.as_str().into(),
        web_link: event.web_link.as_str().into(),
        calendar: app.cal.names.borrow().get(&key).cloned().unwrap_or_default().into(),
        color: app.cal.colors.borrow().get(&key).copied().unwrap_or_else(|| color_of("", 0)),
        cancelled: event.is_cancelled,
        loading: false,
    });
}

pub fn event_body(app: &Rc<App>, account: &str, id: &str, body_html: &str) {
    let Some(ui) = app.ui() else { return };
    let cal = ui.global::<Cal>();
    let mut editor = cal.get_editor();
    if editor.open && editor.account == account && editor.id == id {
        let text = html::render(body_html).text;
        *app.cal.original_body.borrow_mut() = Some((text.clone(), body_html.to_owned()));
        editor.body = text.into();
        cal.set_editor(editor);
    }
}

fn next_half_hour() -> i64 {
    let now = Local::now().timestamp();
    (now / 1800 + 1) * 1800
}

fn default_calendar_index(app: &App) -> i32 {
    let calendars = app.store.calendars().unwrap_or_default();
    let pickable = app.cal.pickable.borrow();
    let default = calendars.iter().find(|(_, calendar, _)| calendar.is_default && calendar.can_edit).map(|(account, calendar, _)| (account.clone(), calendar.id.clone()));
    default
        .and_then(|(account, id)| pickable.iter().position(|candidate| candidate.0 == account && candidate.1 == id))
        .or_else(|| pickable.iter().position(|candidate| candidate.2))
        .unwrap_or(0) as i32
}

fn new_event(app: &Rc<App>, start: i64, end: i64, all_day: bool) {
    let Some(ui) = app.ui() else { return };
    *app.cal.original_body.borrow_mut() = None;
    let (start_local, end_local) = (when::local(start), when::local(end));
    ui.global::<Cal>().set_editor(EventEditor {
        open: true,
        calendar_index: default_calendar_index(app),
        start_date: start_local.format("%Y-%m-%d").to_string().into(),
        start_time: start_local.format("%H:%M").to_string().into(),
        end_date: end_local.format("%Y-%m-%d").to_string().into(),
        end_time: end_local.format("%H:%M").to_string().into(),
        all_day,
        reminder_index: DEFAULT_REMINDER,
        ..EventEditor::default()
    });
}

fn edit_event(app: &Rc<App>) {
    let Some(ui) = app.ui() else { return };
    let Some((account, id)) = app.cal.open.borrow_mut().take() else { return };
    let Ok(Some(event)) = app.store.event(&account, &id) else { return };
    let cal = ui.global::<Cal>();
    cal.set_detail(EventDetail::default());
    let (start_date, start_time, end_date, end_time) = if event.all_day {
        let (first, last) = event_dates(&event);
        (first.format("%Y-%m-%d").to_string(), String::new(), last.format("%Y-%m-%d").to_string(), String::new())
    } else {
        let (start, end) = (when::local(event.start), when::local(event.end));
        (start.format("%Y-%m-%d").to_string(), start.format("%H:%M").to_string(), end.format("%Y-%m-%d").to_string(), end.format("%H:%M").to_string())
    };
    let calendar_index = app
        .cal
        .pickable
        .borrow()
        .iter()
        .position(|candidate| candidate.0 == event.account && candidate.1 == event.calendar_id)
        .unwrap_or(0) as i32;
    let body = if event.body_html.is_empty() { String::new() } else { html::render(&event.body_html).text };
    *app.cal.original_body.borrow_mut() = (!event.body_html.is_empty()).then(|| (body.clone(), event.body_html.clone()));
    cal.set_editor(EventEditor {
        open: true,
        account: account.as_str().into(),
        id: id.as_str().into(),
        calendar_index,
        subject: event.subject.as_str().into(),
        start_date: start_date.into(),
        start_time: start_time.into(),
        end_date: end_date.into(),
        end_time: end_time.into(),
        all_day: event.all_day,
        location: event.location.as_str().into(),
        attendees: format::addresses(&event.attendees.iter().map(|a| a.address.clone()).collect::<Vec<_>>()).into(),
        body: body.into(),
        online: event.is_online,
        busy_index: SHOW_AS.iter().position(|s| *s == event.show_as).unwrap_or(0) as i32,
        reminder_index: DEFAULT_REMINDER,
        saving: false,
        error: SharedString::new(),
    });
    if event.body_html.is_empty() {
        app.engine.send(Request::EventBody { account, id });
    }
}

fn parse_local(date: &str, time: &str) -> Option<i64> {
    let date = NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d").ok()?;
    let time = NaiveTime::parse_from_str(time.trim(), "%H:%M").ok()?;
    Local.from_local_datetime(&date.and_time(time)).earliest().map(|t| t.timestamp())
}

fn editor_error(app: &App, message: &str) {
    if let Some(ui) = app.ui() {
        let cal = ui.global::<Cal>();
        let mut editor = cal.get_editor();
        editor.error = message.into();
        editor.saving = false;
        cal.set_editor(editor);
    }
}

fn save(app: &Rc<App>, editor: EventEditor) {
    let (start, end) = if editor.all_day {
        let first = NaiveDate::parse_from_str(editor.start_date.trim(), "%Y-%m-%d").ok();
        let last = NaiveDate::parse_from_str(editor.end_date.trim(), "%Y-%m-%d").ok().or(first);
        match (first, last) {
            (Some(first), Some(last)) if last >= first => (when::date_seconds(first), when::date_seconds(shift_days(last, 1))),
            _ => return editor_error(app, "Dates are written YYYY-MM-DD, the end on or after the start."),
        }
    } else {
        match (parse_local(&editor.start_date, &editor.start_time), parse_local(&editor.end_date, &editor.end_time)) {
            (Some(start), Some(end)) if end > start => (start, end),
            (Some(_), Some(_)) => return editor_error(app, "The event has to end after it starts."),
            _ => return editor_error(app, "Dates are written YYYY-MM-DD and times HH:MM."),
        }
    };
    let (attendees, invalid) = format::parse_addresses(&editor.attendees);
    if !invalid.is_empty() {
        return editor_error(app, &format!("Not an address: {}", invalid.join(", ")));
    }
    let picked = app.cal.pickable.borrow().get(editor.calendar_index.max(0) as usize).cloned();
    let Some((account, calendar_id)) = (if editor.id.is_empty() {
        match picked {
            Some((account, id, true)) => Some((account, id)),
            Some(_) => return editor_error(app, "Events can't be added to a read-only calendar."),
            None => None,
        }
    } else {
        Some((editor.account.to_string(), picked.map(|c| c.1).unwrap_or_default()))
    }) else {
        return editor_error(app, "There is no calendar to add the event to.");
    };
    let body_html = match &*app.cal.original_body.borrow() {
        Some((text, html)) if text.as_str() == editor.body.as_str() => html.clone(),
        _ => html::text_to_html(&editor.body),
    };
    let draft = EventDraft {
        calendar_id,
        subject: editor.subject.to_string(),
        start,
        end,
        all_day: editor.all_day,
        location: editor.location.to_string(),
        body_html,
        attendees,
        online: editor.online,
        show_as: SHOW_AS.get(editor.busy_index.max(0) as usize).copied().unwrap_or("busy").into(),
        reminder: REMINDERS.get(editor.reminder_index.max(0) as usize).copied().flatten(),
        time_zone: String::new(),
    };
    if let Some(ui) = app.ui() {
        let mut saving = editor.clone();
        saving.saving = true;
        saving.error = SharedString::new();
        ui.global::<Cal>().set_editor(saving);
    }
    let id = (!editor.id.is_empty()).then(|| editor.id.to_string());
    app.engine.send(Request::SaveEvent { account, id, draft });
}

pub fn saved(app: &Rc<App>) {
    if let Some(ui) = app.ui() {
        ui.global::<Cal>().set_editor(EventEditor::default());
    }
    app.status("Event saved");
}

pub fn failed(app: &Rc<App>, error: &str) {
    editor_error(app, &format!("Not saved: {error}"));
}

pub fn key(_app: &Rc<App>, _key: &str, _control: bool, _shift: bool) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(start: i64, end: i64) -> Event {
        Event {
            account: "a".into(),
            id: format!("{start}"),
            calendar_id: "c".into(),
            subject: String::new(),
            start,
            end,
            all_day: false,
            location: String::new(),
            organizer: Default::default(),
            attendees: vec![],
            preview: String::new(),
            body_html: String::new(),
            response: Response::Organizer,
            show_as: "busy".into(),
            is_cancelled: false,
            is_online: false,
            join_url: String::new(),
            is_recurring: false,
            web_link: String::new(),
        }
    }

    #[test]
    fn overlapping_events_share_the_day() {
        let day = NaiveDate::from_ymd_opt(2026, 10, 21).unwrap();
        let base = when::local_midnight(day);
        let events = vec![
            event(base + 9 * 3600, base + 10 * 3600),
            event(base + 9 * 3600 + 1800, base + 11 * 3600),
            event(base + 12 * 3600, base + 13 * 3600),
        ];
        let (boxes, strip, rows) = layout(&[day], &events, &HashMap::new());
        assert!(strip.is_empty() && rows == 0);
        let columns: Vec<_> = boxes.iter().map(|b| (b.column, b.columns)).collect();
        assert_eq!(columns, [(0, 2), (1, 2), (0, 1)]);
        assert!((boxes[0].top - 9.0 / 24.0).abs() < 0.01);
    }

    #[test]
    fn long_events_go_in_the_strip() {
        let days: Vec<_> = (0..7).map(|d| NaiveDate::from_ymd_opt(2026, 10, 19 + d).unwrap()).collect();
        let mut trip = event(when::date_seconds(days[1]), when::date_seconds(days[4]));
        trip.all_day = true;
        let (boxes, strip, rows) = layout(&days, &[trip], &HashMap::new());
        assert!(boxes.is_empty());
        assert_eq!((strip[0].first_day, strip[0].days, rows), (1, 3, 1));
    }

    #[test]
    fn weeks_start_on_monday() {
        let thursday = NaiveDate::from_ymd_opt(2026, 10, 22).unwrap();
        let days = visible_days(1, thursday);
        assert_eq!(days.len(), 5);
        assert_eq!(days[0], NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
    }
}
