//! Signs an account in for a data folder, as the app does, so that
//! `examples/live.rs` can reach the server from a computer whose data folder
//! was copied from another (where the refresh token can't be read).
//!
//!   LOOKOUT_DATA_DIR=<data folder> sign_in [rules]
//!
//! The refresh token is kept as the app keeps it here: in the keyring, or a
//! file only the user can read in `<data folder>/tokens`. With `rules`, the
//! permission to write inbox rules and Focused Inbox overrides is asked for
//! as well, so a run doesn't stop on it.

use lookout_core::{Config, Store, auth};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let rules = std::env::args().nth(1).as_deref() == Some("rules");
    let config = Config::load();
    let client_id = config.client_id().expect("an application id");
    let http = lookout_core::reqwest::Client::new();
    println!("Sign in in the browser…");
    let (session, account) = auth::sign_in(http, &client_id).await.expect("signing in");
    println!("Signed in as {} <{}>, id {}", account.name, account.email, account.id);
    let store = Store::open(&Store::default_path().expect("a data folder")).expect("the cache");
    store.put_account(&account).expect("keeping the account");
    if rules {
        session
            .allow_rules(&account.email, || println!("Allow writing rules in the browser…"))
            .await
            .expect("allowing rules");
        println!("Rules allowed");
    }
}
