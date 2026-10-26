//! The Outlook inbox rule Lookout keeps for blocked senders: mail from them
//! goes to Junk Email, on the server, whether Lookout is running or not.
//! It shows among the account's rules in Outlook, and can be changed there.

use std::sync::Mutex;

use serde::Deserialize;
use serde_json::json;

use crate::graph::encode;
use crate::{Graph, Result, Store};

pub const RULE_NAME: &str = "Lookout: blocked senders";
const RULES: &str = "/me/mailFolders/inbox/messageRules";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Rule {
    id: String,
    display_name: Option<String>,
    #[serde(default)]
    conditions: Conditions,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Conditions {
    #[serde(default)]
    from_addresses: Vec<crate::model::wire::Recipient>,
}

#[derive(Deserialize)]
struct Folder {
    id: String,
}

/// Adds `addresses` to the blocked senders, or with `blocked` false takes
/// them off. Writing the rule needs a permission asked for the first time:
/// `email` is the account's address, and `asking` is called if the browser
/// opens for it.
pub async fn block_senders(
    graph: &Graph,
    store: &Mutex<Store>,
    addresses: &[String],
    blocked: bool,
    email: &str,
    asking: impl FnOnce(),
) -> Result<()> {
    let rules: Vec<Rule> = graph.all(RULES).await?;
    let rule = rules.into_iter().find(|rule| rule.display_name.as_deref() == Some(RULE_NAME));
    let before: Vec<String> = rule
        .iter()
        .flat_map(|rule| &rule.conditions.from_addresses)
        .filter_map(|recipient| recipient.email_address.address.clone())
        .collect();
    let after = blocked_after(&before, addresses, blocked);
    if after == before {
        return Ok(());
    }
    graph.session().allow_rules(email, asking).await?;
    let from: Vec<_> = after.iter().map(|address| json!({ "emailAddress": { "address": address } })).collect();
    match rule {
        Some(rule) if after.is_empty() => graph.delete(&format!("{RULES}/{}", encode(&rule.id))).await,
        Some(rule) => {
            graph
                .patch_empty(&format!("{RULES}/{}", encode(&rule.id)), &json!({ "conditions": { "fromAddresses": from } }))
                .await
        }
        None => {
            let junk = junk_folder(graph, store).await?;
            graph
                .post_empty(
                    RULES,
                    Some(&json!({
                        "displayName": RULE_NAME,
                        "sequence": 1,
                        "isEnabled": true,
                        "conditions": { "fromAddresses": from },
                        "actions": { "moveToFolder": junk, "stopProcessingRules": true },
                    })),
                )
                .await
        }
    }
}

/// The blocked senders once `addresses` are added or taken off, matching
/// regardless of case.
fn blocked_after(before: &[String], addresses: &[String], blocked: bool) -> Vec<String> {
    let listed = |list: &[String], address: &str| list.iter().any(|other| other.eq_ignore_ascii_case(address));
    if blocked {
        let mut after = before.to_vec();
        for address in addresses {
            if !address.is_empty() && !listed(&after, address) {
                after.push(address.clone());
            }
        }
        after
    } else {
        before.iter().filter(|address| !listed(addresses, address)).cloned().collect()
    }
}

/// The Junk Email folder's id, which a rule needs rather than its name.
async fn junk_folder(graph: &Graph, store: &Mutex<Store>) -> Result<String> {
    let known = store.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).well_known_folder(graph.account_id(), "junkemail")?;
    match known {
        Some(id) => Ok(id),
        None => Ok(graph.get::<Folder>("/me/mailFolders/junkemail?$select=id").await?.id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn senders_are_added_once_and_taken_off_regardless_of_case() {
        let before = strings(&["ads@x.test"]);
        assert_eq!(blocked_after(&before, &strings(&["ADS@x.test", "spam@y.test", ""]), true), strings(&["ads@x.test", "spam@y.test"]));
        assert_eq!(blocked_after(&before, &strings(&["Ads@X.test"]), false), Vec::<String>::new());
        assert_eq!(blocked_after(&before, &strings(&["other@x.test"]), false), before);
    }
}
