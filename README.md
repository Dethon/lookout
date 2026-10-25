# Lookout

A fast, light desktop client for Outlook.com and Microsoft 365 mail, calendars
and contacts, written in Rust. It talks to Microsoft Graph, keeps a local
SQLite cache, and draws its interface natively with [Slint](https://slint.dev)'s
software renderer: no browser engine in the main window.

On Windows it idles at about 20 MB of private memory (welcome screen, before
signing in), against the hundreds the new Outlook takes.

## Features

- **Accounts**: personal (Outlook.com, Hotmail, Live) and work or school
  (Microsoft 365), several at once. Sign-in in the browser; refresh tokens
  are encrypted with DPAPI on Windows, in the keyring elsewhere.
- **Mail**: folders and subfolders with unread counts, collapsible, with
  Favourites on top (right-click a folder); a message list grouped by
  Today, Yesterday, weekday, and older, that stays light at any size (only
  row ids are held, rows are read on demand); Ctrl/Shift multi-select with
  bulk read, flag, archive, move and delete sent in batches; drag messages
  onto a folder; reading pane with native rich text (bold, italic, links,
  colours adjusted to the theme, lists, quotes, headings, pictures side by
  side as the sender laid them out); remote pictures blocked until allowed;
  attachments to open or save; reply, reply all, forward (threaded, with the
  original quoted by the server); new mail with Cc/Bcc, a formatting
  toolbar (bold, italic, strikethrough, lists, quotes, links, written as
  markdown) with a preview, per-account signatures, attachments of any
  size, importance and address completion (contacts, people who wrote to
  you and people you wrote to, those you write to most first; arrow keys
  and Enter or Tab pick one); drafts; instant offline search (SQLite FTS5)
  and server search for older mail; unread-only filter; new mail notices.
- **Layout**: the folder pane and the message list are resized by dragging
  their edges, and kept that way; the folder pane can be put away into a
  strip of folder icons with unread badges (Ctrl+B), and gives way to it by
  itself when the window is narrow, opening over the list when wanted. A
  message can take the whole window. Toolbars drop their labels, then move
  the least used actions into a menu, as the room shrinks. The calendars'
  sidebar folds away the same way.
- **People's faces**: contacts' pictures, fetched in the background and
  kept in the cache, and everyone else's initials in a colour of their own,
  in the message list (Settings can hide them there), the reading pane,
  address completion and People. Over a message, its sender's picture
  turns into a check box to pick several. A click on a message's sender
  shows their card: write to them, or add them to your contacts.
- **Original view**: messages are shown as sent, laid out and painted by
  [Blitz](https://github.com/DioxusLabs/blitz) inside the reading pane:
  about 15-25 MB more than the simplified view, against some 220 MB for
  WebView2. Only the part in view is painted. Scripts never run; remote
  pictures, styles and fonts follow the message's setting. The *Original*
  switch turns it off for the simplified, theme-coloured view, which is
  also used should Blitz fail on a message. To compare with a browser,
  `"webview": true` in `config.json` shows the original in WebView2
  instead, on Windows.
- **Calendar**: day, work week, week and month views across calendars and
  accounts, with overlap layout and all-day strip, and a month in the
  sidebar to go to any day; create, edit, delete and cancel events;
  attendees, location, Teams meetings, show-as, reminders; accept,
  tentative, decline; join online meetings.
- **People**: contacts across accounts, search, create, edit, delete, mail
  a contact; each card lists the latest messages with that person, a click
  away.
- **Windows**: new mail in the Inbox shows in Windows notifications while
  the window is in the background, and a click on one opens the message;
  closing the window leaves Lookout in the notification area, where a
  click on its icon brings it back and its menu quits; and it can start
  when you sign in, waiting there. Settings turns each on or off.
- Light and dark themes; keyboard shortcuts.
- **Updates**: Lookout looks for a new release when it starts and once a
  day (Settings can turn that off). Installed with the Windows installer,
  it downloads the new one ahead, checks it against the release's
  checksums, and installs it when you click *Restart to update*; otherwise
  it links to the release.

### Shortcuts

| Key | Action |
| --- | --- |
| ↑ / ↓, Home / End | Previous / next message |
| Ctrl+N | New mail |
| Ctrl+R / Ctrl+Shift+R | Reply / reply all |
| Ctrl+F | Forward |
| Ctrl+Q / Ctrl+U | Mark read / unread |
| Delete | Delete |
| Ctrl+E | Search |
| Ctrl+A, Shift+↑/↓, Ctrl/Shift+click | Select several messages |
| Ctrl+B / Ctrl+I / Ctrl+K | Bold / italic / link, while writing |
| Ctrl+Enter | Send |
| Ctrl+1 / 2 / 3 | Mail / calendar / people |
| Ctrl+B | Show or hide the folder pane (bold in a message's text) |
| Esc | Leave the whole-window message, close a menu or dialog |
| F5 | Sync now |

### Not there yet

Compared with Outlook: true WYSIWYG composing (formatting is written as
markdown, with a preview), conversation view, Focused Inbox, rules, categories (shown, not edited),
shared and delegated mailboxes and calendars, editing a whole recurring
series (single occurrences can be edited), scheduling assistant, and offline
sending queue.

## Install

Download from [Releases](https://github.com/Dethon/lookout/releases):

- **Windows**: run `Lookout-<version>-setup.exe`, which installs for your
  user without administrator rights; uninstall from Settings › Apps. It is
  not code-signed, so SmartScreen warns on first run: *More info*, then
  *Run anyway*. `/S` installs silently. Or unzip
  `lookout-<version>-windows-x64.zip` anywhere and run `lookout.exe`.
- **Debian, Ubuntu**: `sudo apt install ./lookout_<version>_amd64.deb`.
- **Other Linux**: unpack `lookout-<version>-linux-x64.tar.gz` and run
  `./install.sh`, which installs to `~/.local`.

Lookout updates itself from then on; see *Updates* above.

Uninstalling keeps your accounts, cache and settings (see *Build* for
where), so a reinstall picks up where you left off.

## Signing in

Lookout signs in with its own Entra app registration (a public client, so its
id is no secret and is built in). Click *Sign in with Microsoft* and sign in
in the browser. It asks for `Mail.ReadWrite Mail.Send Calendars.ReadWrite
Contacts.ReadWrite MailboxSettings.Read User.Read offline_access`; work
accounts whose organisation restricts user consent need an administrator to
approve these once.

To use a registration of your own instead, set its id in Settings, or build
with `LOOKOUT_CLIENT_ID=<id>`. Register it in the
[Microsoft Entra admin center](https://entra.microsoft.com) under *App
registrations*, for **accounts in any organizational directory and personal
Microsoft accounts**, with the redirect URI `http://localhost` on the
**Public client/native (mobile & desktop)** platform and no client secret.

## Build

Rust stable (see `rust-toolchain.toml`). On Linux Slint needs
`libfontconfig1-dev`, and running it needs `libxkbcommon-x11-0`.

    cargo run --release -p lookout

Windows builds cross-compile from Linux with
[cargo-xwin](https://github.com/rust-cross/cargo-xwin), with no C toolchain:
on Windows Lookout uses the SQLite that comes with the system
(`winsqlite3.dll`), linked through an import library `crates/core/build.rs`
writes from `winsqlite3.def`.

    cargo xwin build --release --target x86_64-pc-windows-msvc -p lookout

`scripts/package.py` builds both and writes the packages to `dist/`. The
Windows installer is defined in `installer/lookout.nsi` and built with
[NSIS](https://nsis.sourceforge.io); without `makensis` installed the script
unpacks it into `target/nsis`, no root needed. Set `PUBLISHER="Your Name"`
to name a publisher in it.

The icon is drawn in `crates/app/assets/icon.svg`; after changing it,
`cargo run -p lookout --example icon` regenerates the PNG, the `.ico` and the
installer's side panel.

Data lives in `%LOCALAPPDATA%\lookout` (`~/.local/share/lookout` on Linux):
`cache.sqlite3`, `config.json` and `lookout.log`. `LOOKOUT_DATA_DIR`
overrides the folder.

### Checking the interface

`cargo run -p lookout --example snapshot -- out.png` renders the window with
sample data, without a display; `SNAPSHOT_PAGE=compose|calendar|month|people|welcome|settings`
and `SNAPSHOT_DARK=1` pick what to show, a width and height after the file
name try other window sizes, and `SNAPSHOT_COLLAPSED`, `SNAPSHOT_FLYOUT`,
`SNAPSHOT_FULL`, `SNAPSHOT_CLICK=x,y` and `SNAPSHOT_DRAG=x,y,x2` show the
panes put away, the folders over the list, a message across the window, a
click, and a pane being resized.

## How it is put together

- `crates/core` (`lookout-core`): no interface code.
  - `auth`: authorization code flow with PKCE on a loopback port.
  - `graph`: requests with throttling retries, paging, and immutable ids
    (a moved message keeps its id, so the cache sees a move).
  - `sync`: folders listed whole; messages by per-folder delta queries,
    resumable page by page; calendars by window; contacts.
  - `store`: SQLite in WAL mode with a small page cache. Bodies are fetched
    when a message is opened and the least recently read are dropped past
    64 MB; their text stays searchable.
  - `html`: turns mail HTML into blocks of Slint's `StyledText` markdown,
    escaping the text so nothing in it is read as markup.
  - `mail`, `calendar`, `people`: the changes the user makes.
  - Contacts' pictures are fetched after contacts are synced, a hundred
    at a time, shrunk to 144 pixels and kept in the cache; one found
    missing is asked for again after a week.
- `crates/app` (`lookout`): the Slint interface (`ui/`), a lazy message
  list model, and an engine thread (two Tokio workers) that owns the
  network and the cache's writer, syncs every minute, and asks each Inbox
  for changes every five seconds, so new mail shows at once.

`cargo test --workspace` includes a run against a stand-in Graph server
covering paging, delta links, body caching, attachments,
calendars, contacts and throttling.
