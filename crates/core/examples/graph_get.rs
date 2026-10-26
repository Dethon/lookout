//! Reads a Graph path as a signed-in account of a data folder and prints the
//! JSON, to check what a change did on the server. Only reads.
//!
//!   LOOKOUT_DATA_DIR=<data folder> graph_get <path>
//!
//! For instance `/me/mailFolders/inbox/messages?$top=1&$select=subject,inferenceClassification`.

use std::sync::Arc;

use lookout_core::{Config, Graph, Session, Store};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let path = std::env::args().nth(1).expect("a path such as /me");
    let client_id = Config::load().client_id().expect("an application id");
    let store = Store::open(&Store::default_path().expect("a data folder")).expect("the cache");
    let account = store.accounts().expect("the accounts").into_iter().next().expect("a signed-in account");
    let http = lookout_core::reqwest::Client::new();
    let session = Session::restore(http.clone(), &client_id, &account.id).expect("a saved sign-in; run sign_in");
    let graph = Graph::new(http, Arc::new(session));
    let value: serde_json::Value = graph.get(&path).await.expect("the request");
    println!("{}", serde_json::to_string_pretty(&value).expect("JSON"));
}
