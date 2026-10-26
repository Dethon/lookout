//! Telling the user about new mail while the window is in the background:
//! Windows notifications, which a click on brings the message up. Elsewhere
//! the in-window notice is all there is.

use lookout_core::model::MessageSummary;

// More arriving at once than this get one notification between them.
#[cfg(windows)]
const ONE_EACH: usize = 3;

/// Whether Windows notifications are there to show.
pub const AVAILABLE: bool = cfg!(windows);

/// Shows `messages` in Windows notifications.
#[cfg(windows)]
pub fn new_mail(messages: &[MessageSummary]) {
    if let Err(error) = toasts::show(messages) {
        log::warn!("couldn't show a notification: {error}");
    }
}

#[cfg(not(windows))]
pub fn new_mail(_messages: &[MessageSummary]) {}

/// The notifications' title and lines, and the message a click opens.
#[cfg_attr(not(windows), allow(dead_code))]
fn notices(messages: &[MessageSummary], one_each: usize) -> Vec<(Vec<String>, &MessageSummary)> {
    let subject = |message: &MessageSummary| {
        if message.subject.trim().is_empty() { "(no subject)".to_owned() } else { message.subject.clone() }
    };
    match messages {
        [] => Vec::new(),
        few if few.len() <= one_each => few
            .iter()
            .map(|message| (vec![message.from.display().to_owned(), subject(message), message.preview.clone()], message))
            .collect(),
        many => {
            let mut senders: Vec<&str> = Vec::new();
            for message in many {
                let sender = message.from.display();
                if !senders.contains(&sender) {
                    senders.push(sender);
                }
            }
            let from = match senders.as_slice() {
                [one] => format!("From {one}"),
                [first, second] => format!("From {first} and {second}"),
                [first, second, rest @ ..] => format!("From {first}, {second} and {} more", rest.len()),
                [] => String::new(),
            };
            vec![(vec![format!("{} new messages", many.len()), from], &many[0])]
        }
    }
}

#[cfg(windows)]
mod toasts {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use lookout_core::model::MessageSummary;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Foundation::TypedEventHandler;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager, ToastNotifier};
    use windows::core::{HSTRING, IInspectable, Ref};

    // Names Lookout to Windows, which shows the name and icon registered
    // for it on the notifications.
    const APP_ID: &str = "Dethon.Lookout";
    const APP_KEY: &str = r"Software\Classes\AppUserModelId\Dethon.Lookout";
    const ICON: &[u8] = include_bytes!("../assets/icon.png");
    // Notifications whose clicks are listened for: a click on an older one
    // in the notification centre still brings the window forward.
    const KEPT: usize = 20;

    thread_local! {
        static NOTIFIER: RefCell<Option<ToastNotifier>> = const { RefCell::new(None) };
        static SHOWN: RefCell<VecDeque<ToastNotification>> = const { RefCell::new(VecDeque::new()) };
    }

    pub fn show(messages: &[MessageSummary]) -> windows::core::Result<()> {
        let notifier = NOTIFIER.with(|notifier| -> windows::core::Result<ToastNotifier> {
            let mut notifier = notifier.borrow_mut();
            if notifier.is_none() {
                *notifier = Some(start()?);
            }
            Ok(notifier.clone().expect("just set"))
        })?;
        for (lines, message) in super::notices(messages, super::ONE_EACH) {
            let document = XmlDocument::new()?;
            document.LoadXml(&HSTRING::from(xml(&lines)))?;
            let toast = ToastNotification::CreateToastNotification(&document)?;
            let (account, folder, id) = (message.account.clone(), message.folder_id.clone(), message.id.clone());
            toast.Activated(&TypedEventHandler::new(move |_: Ref<ToastNotification>, _: Ref<IInspectable>| {
                let (account, folder, id) = (account.clone(), folder.clone(), id.clone());
                _ = slint::invoke_from_event_loop(move || {
                    crate::app::with_app(|app| app.show_message(&account, &folder, &id));
                });
                Ok(())
            }))?;
            notifier.Show(&toast)?;
            SHOWN.with(|shown| {
                let mut shown = shown.borrow_mut();
                shown.push_back(toast);
                if shown.len() > KEPT {
                    shown.pop_front();
                }
            });
        }
        Ok(())
    }

    /// Readies Windows' notifications on the first one, so a session without
    /// new mail never loads them.
    fn start() -> windows::core::Result<ToastNotifier> {
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};

        // The single-threaded apartment winit sets up for the window: started
        // in the tray, the window may not be there yet, and winit panics on
        // finding another kind. Already set up, this returns `S_FALSE`.
        // SAFETY: no reserved pointer; this thread runs the window's events.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok()?;
        register();
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))
    }

    /// The name and icon Windows shows on the notifications, for a program
    /// that isn't a packaged app. The uninstaller removes them.
    fn register() {
        let icon = lookout_core::config::data_dir().map(|dir| dir.join("icon.png"));
        let icon = icon.ok().filter(|path| std::fs::read(path).is_ok_and(|bytes| bytes == ICON) || std::fs::write(path, ICON).is_ok());
        let mut values = vec![("DisplayName", "Lookout".to_owned())];
        if let Some(icon) = icon {
            values.push(("IconUri", icon.display().to_string()));
        }
        for (name, value) in values {
            if let Err(error) = crate::registry::set(APP_KEY, name, &value) {
                log::warn!("registering for notifications: {error}");
            }
        }
    }

    fn xml(lines: &[String]) -> String {
        let texts: String = lines.iter().map(|line| format!("<text>{}</text>", escape(line))).collect();
        format!(
            "<toast><visual><binding template=\"ToastGeneric\">{texts}</binding></visual>\
             <audio src=\"ms-winsoundevent:Notification.Mail\"/></toast>"
        )
    }

    fn escape(text: &str) -> String {
        let mut escaped = String::with_capacity(text.len());
        for c in text.chars() {
            match c {
                '&' => escaped.push_str("&amp;"),
                '<' => escaped.push_str("&lt;"),
                '>' => escaped.push_str("&gt;"),
                '"' => escaped.push_str("&quot;"),
                '\'' => escaped.push_str("&apos;"),
                // Not allowed in XML at all.
                c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
                c => escaped.push(c),
            }
        }
        escaped
    }
}

#[cfg(test)]
mod tests {
    use lookout_core::model::{Address, Flag, MessageSummary};

    fn from(name: &str, subject: &str) -> MessageSummary {
        MessageSummary {
            account: "a".into(),
            id: subject.into(),
            folder_id: "inbox".into(),
            conversation_id: String::new(),
            subject: subject.into(),
            from: Address { name: name.into(), address: format!("{name}@example.com") },
            to: Vec::new(),
            preview: String::new(),
            received: 0,
            is_read: false,
            is_draft: false,
            flag: Flag::default(),
            high_importance: false,
            has_attachments: false,
            focused: true,
        }
    }

    #[test]
    fn a_few_are_shown_one_each() {
        let messages = [from("Ana", "Lunch"), from("Ben", "")];
        let notices = super::notices(&messages, 3);
        assert_eq!(notices.len(), 2);
        assert_eq!(notices[0].0[..2], ["Ana".to_owned(), "Lunch".to_owned()]);
        assert_eq!(notices[1].0[1], "(no subject)");
    }

    #[test]
    fn many_are_shown_together() {
        let messages = [from("Ana", "a"), from("Ben", "b"), from("Ana", "c"), from("Cy", "d"), from("Di", "e")];
        let notices = super::notices(&messages, 3);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].0, ["5 new messages", "From Ana, Ben and 2 more"]);
        assert_eq!(notices[0].1.subject, "a", "a click opens the first");
    }
}
