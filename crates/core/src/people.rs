//! Creating, changing and deleting contacts.

use std::sync::Mutex;

use serde_json::json;

use crate::graph::encode;
use crate::model::{Contact, wire};
use crate::{Graph, Result, Store, sync};

fn lock(store: &Mutex<Store>) -> std::sync::MutexGuard<'_, Store> {
    store.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn fields(contact: &Contact) -> serde_json::Value {
    let numbers = |kind: &str| -> Vec<&str> {
        contact.phones.iter().filter(|phone| phone.kind == kind).map(|phone| phone.number.as_str()).collect()
    };
    let display_name = if contact.display_name.trim().is_empty() {
        format!("{} {}", contact.given_name, contact.surname).trim().to_owned()
    } else {
        contact.display_name.clone()
    };
    json!({
        "displayName": display_name,
        "givenName": contact.given_name,
        "surname": contact.surname,
        "emailAddresses": contact.emails.iter().map(|email| json!({
            "address": email.address,
            "name": if email.name.is_empty() { &display_name } else { &email.name },
        })).collect::<Vec<_>>(),
        "mobilePhone": numbers("mobile").first().copied().unwrap_or_default(),
        "businessPhones": numbers("business"),
        "homePhones": numbers("home"),
        "companyName": contact.company,
        "jobTitle": contact.job_title,
        "personalNotes": contact.notes,
    })
}

pub async fn create(graph: &Graph, store: &Mutex<Store>, contact: &Contact) -> Result<Contact> {
    let created: wire::Contact = graph.post("/me/contacts", &fields(contact)).await?;
    let contact = sync::contact_from(graph.account_id(), &created);
    lock(store).put_contacts(std::slice::from_ref(&contact))?;
    Ok(contact)
}

pub async fn update(graph: &Graph, store: &Mutex<Store>, contact: &Contact) -> Result<Contact> {
    let updated: wire::Contact = graph.patch(&format!("/me/contacts/{}", encode(&contact.id)), &fields(contact)).await?;
    let contact = sync::contact_from(graph.account_id(), &updated);
    lock(store).put_contacts(std::slice::from_ref(&contact))?;
    Ok(contact)
}

pub async fn delete(graph: &Graph, store: &Mutex<Store>, id: &str) -> Result<()> {
    lock(store).remove_contacts(graph.account_id(), &[id.to_owned()])?;
    graph.delete(&format!("/me/contacts/{}", encode(id))).await
}
