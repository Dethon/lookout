//! A log in the data folder, kept short.

use std::io::Write;
use std::sync::Mutex;

// Past this the log starts over at the next launch.
const MAX_LOG_BYTES: u64 = 1024 * 1024;

struct Logger {
    file: Option<Mutex<std::fs::File>>,
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info || metadata.target().starts_with("lookout")
            && metadata.level() <= log::Level::Debug
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} {:5} {}: {}\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            record.level(),
            record.target(),
            record.args()
        );
        if let Some(file) = &self.file
            && let Ok(mut file) = file.lock()
        {
            _ = file.write_all(line.as_bytes());
        }
        #[cfg(debug_assertions)]
        eprint!("{line}");
    }

    fn flush(&self) {}
}

pub fn init() {
    let file = lookout_core::config::data_dir().ok().and_then(|dir| {
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("lookout.log");
        let append = std::fs::metadata(&path).is_ok_and(|meta| meta.len() < MAX_LOG_BYTES);
        std::fs::OpenOptions::new()
            .create(true)
            .append(append)
            .write(true)
            .truncate(!append)
            .open(path)
            .ok()
    });
    let logger = Box::leak(Box::new(Logger { file: file.map(Mutex::new) }));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(log::LevelFilter::Debug);
    }
}
