//! The people page's behaviour.

use std::cell::RefCell;
use std::rc::Rc;

use lookout_core::model::{Address, Contact, Phone};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::app::App;
use crate::engine::Request;
use crate::{ContactEditor, ContactRow, ContactView, MainWindow, People, format, mail_ui};

#[derive(Default)]
pub struct PeopleState {
    contacts: RefCell<Vec<Contact>>,
    selected: RefCell<Option<(String, String)>>,
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
    let people = ui.global::<People>();
    people.on_filter_edited(with_app!(app, |app, _text| refresh(&app)));
    people.on_select(with_app!(app, |app, index| select(&app, index)));
    people.on_new_contact(with_app!(app, |app| {
        if let Some(ui) = app.ui() {
            ui.global::<People>().set_editor(ContactEditor { open: true, ..ContactEditor::default() });
        }
    }));
    people.on_edit_contact(with_app!(app, |app| edit(&app)));
    people.on_delete_contact(with_app!(app, |app| {
        let Some((account, id)) = app.people.selected.borrow_mut().take() else { return };
        app.engine.send(Request::DeleteContact { account, id });
        if let Some(ui) = app.ui() {
            ui.global::<People>().set_contact(ContactView::default());
        }
    }));
    people.on_email(with_app!(app, |app, address| {
        let (to, _) = format::parse_addresses(&address);
        mail_ui::new_message(&app, to, String::new());
    }));
    people.on_save_contact(with_app!(app, |app, editor| save(&app, editor)));
    people.on_cancel_editor(with_app!(app, |app| {
        if let Some(ui) = app.ui() {
            ui.global::<People>().set_editor(ContactEditor::default());
        }
    }));
}

/// Opens the editor for a new contact with this address, as for the sender
/// of a message.
pub fn new_contact_for(app: &Rc<App>, account: &str, person: &Address) {
    let Some(ui) = app.ui() else { return };
    // An address alone has no name worth keeping.
    let name = if person.name == person.address { "" } else { person.name.trim() };
    let (given, surname) = match name.rsplit_once(' ') {
        Some((given, surname)) if !name.contains(',') => (given.trim(), surname.trim()),
        _ => (name, ""),
    };
    ui.global::<People>().set_editor(ContactEditor {
        open: true,
        account_index: app.account_index(account),
        given_name: given.into(),
        surname: surname.into(),
        display_name: name.into(),
        emails: person.address.as_str().into(),
        ..ContactEditor::default()
    });
}

/// Shows a contact's card on the People page.
pub fn show_contact(app: &Rc<App>, account: &str, id: &str) {
    let Some(ui) = app.ui() else { return };
    let people = ui.global::<People>();
    people.set_filter("".into());
    *app.people.selected.borrow_mut() = Some((account.to_owned(), id.to_owned()));
    ui.global::<crate::Shell>().set_page(crate::Page::People);
    refresh(app);
}

pub fn refresh(app: &Rc<App>) {
    let Some(ui) = app.ui() else { return };
    let people = ui.global::<People>();
    let contacts = app.store.contacts(people.get_filter().as_str()).unwrap_or_default();
    let rows: Vec<ContactRow> = contacts
        .iter()
        .map(|contact| ContactRow {
            account: contact.account.as_str().into(),
            id: contact.id.as_str().into(),
            name: name_of(contact).into(),
            email: contact.emails.first().map(|e| e.address.as_str()).unwrap_or_default().into(),
            initials: format::initials(&name_of(contact)).into(),
            hue: hue_of(contact),
            picture: app.photos.for_contact(&app.store, &contact.account, &contact.id),
            detail: contact
                .emails
                .first()
                .map(|e| e.address.clone())
                .or_else(|| (!contact.company.is_empty()).then(|| contact.company.clone()))
                .unwrap_or_default()
                .into(),
        })
        .collect();
    let selected = app.people.selected.borrow().clone();
    let index = selected.and_then(|(account, id)| contacts.iter().position(|c| c.account == account && c.id == id));
    people.set_contacts(ModelRc::new(VecModel::from(rows)));
    *app.people.contacts.borrow_mut() = contacts;
    match index {
        Some(index) => select(app, index as i32),
        None => {
            people.set_selected(-1);
            people.set_contact(ContactView::default());
        }
    }
}

fn name_of(contact: &Contact) -> String {
    if !contact.display_name.trim().is_empty() {
        contact.display_name.clone()
    } else if let Some(email) = contact.emails.first() {
        email.address.clone()
    } else {
        "(no name)".into()
    }
}

/// The colour of a contact's initials: their address's, as in mail.
fn hue_of(contact: &Contact) -> i32 {
    format::hue(contact.emails.first().map(|email| email.address.as_str()).unwrap_or(&contact.display_name))
}

fn select(app: &Rc<App>, index: i32) {
    let Some(ui) = app.ui() else { return };
    let contacts = app.people.contacts.borrow();
    let Some(contact) = contacts.get(index.max(0) as usize) else { return };
    *app.people.selected.borrow_mut() = Some((contact.account.clone(), contact.id.clone()));
    let mut lines: Vec<SharedString> = Vec::new();
    let role = [contact.job_title.as_str(), contact.company.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    if !role.is_empty() {
        lines.push(role.into());
    }
    for phone in &contact.phones {
        let kind = match phone.kind.as_str() {
            "mobile" => "Mobile",
            "business" => "Work",
            _ => "Home",
        };
        lines.push(format!("{kind}: {}", phone.number).into());
    }
    let emails: Vec<SharedString> = contact.emails.iter().map(|email| email.address.as_str().into()).collect();
    let people = ui.global::<People>();
    people.set_selected(index);
    people.set_contact(ContactView {
        shown: true,
        account: contact.account.as_str().into(),
        id: contact.id.as_str().into(),
        name: name_of(contact).into(),
        initials: format::initials(&name_of(contact)).into(),
        hue: hue_of(contact),
        picture: app.photos.for_contact(&app.store, &contact.account, &contact.id),
        lines: ModelRc::new(VecModel::from(lines)),
        emails: ModelRc::new(VecModel::from(emails)),
        notes: contact.notes.as_str().into(),
    });
}

fn edit(app: &Rc<App>) {
    let Some(ui) = app.ui() else { return };
    let Some((account, id)) = app.people.selected.borrow().clone() else { return };
    let Ok(Some(contact)) = app.store.contact(&account, &id) else { return };
    let phone = |kind: &str| -> SharedString {
        contact.phones.iter().filter(|p| p.kind == kind).map(|p| p.number.as_str()).collect::<Vec<_>>().join("; ").into()
    };
    ui.global::<People>().set_editor(ContactEditor {
        open: true,
        account_index: app.account_index(&account),
        id: id.as_str().into(),
        given_name: contact.given_name.as_str().into(),
        surname: contact.surname.as_str().into(),
        display_name: contact.display_name.as_str().into(),
        emails: format::addresses(&contact.emails).into(),
        mobile: phone("mobile"),
        business: phone("business"),
        home: phone("home"),
        company: contact.company.as_str().into(),
        job_title: contact.job_title.as_str().into(),
        notes: contact.notes.as_str().into(),
        saving: false,
    });
}

fn save(app: &Rc<App>, editor: ContactEditor) {
    let Some(ui) = app.ui() else { return };
    let (emails, invalid) = format::parse_addresses(&editor.emails);
    if !invalid.is_empty() {
        app.toast(&format!("Not an address: {}", invalid.join(", ")));
        return;
    }
    let account = if editor.id.is_empty() {
        app.account_at(editor.account_index).map(|a| a.id)
    } else {
        app.people.selected.borrow().as_ref().map(|(account, _)| account.clone())
    };
    let Some(account) = account else { return };
    let mut phones = Vec::new();
    for (kind, numbers) in [("mobile", &editor.mobile), ("business", &editor.business), ("home", &editor.home)] {
        for number in numbers.split(';').map(str::trim).filter(|n| !n.is_empty()) {
            phones.push(Phone { kind: kind.into(), number: number.into() });
        }
    }
    let contact = Contact {
        account,
        id: editor.id.to_string(),
        display_name: editor.display_name.trim().to_owned(),
        given_name: editor.given_name.trim().to_owned(),
        surname: editor.surname.trim().to_owned(),
        emails,
        phones,
        company: editor.company.trim().to_owned(),
        job_title: editor.job_title.trim().to_owned(),
        notes: editor.notes.to_string(),
    };
    if contact.display_name.is_empty() && contact.given_name.is_empty() && contact.surname.is_empty() && contact.emails.is_empty() {
        app.toast("Give the contact a name or an address");
        return;
    }
    let mut saving = editor;
    saving.saving = true;
    ui.global::<People>().set_editor(saving);
    app.engine.send(Request::SaveContact { contact });
}

pub fn saved(app: &Rc<App>) {
    if let Some(ui) = app.ui() {
        ui.global::<People>().set_editor(ContactEditor::default());
    }
    app.status("Contact saved");
}

pub fn failed(app: &Rc<App>, error: &str) {
    if let Some(ui) = app.ui() {
        let people = ui.global::<People>();
        let mut editor = people.get_editor();
        editor.saving = false;
        people.set_editor(editor);
    }
    app.toast(&format!("Not saved: {error}"));
}
