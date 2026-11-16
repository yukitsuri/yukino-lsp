use std::collections::HashMap;
use std::fs;
use std::io::{stdout, Write};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, SystemTime};

use crate::check::run_check;

pub fn run_watch(path: &Path, delay_ms: u64, json_mode: bool) {
    let delay = Duration::from_millis(delay_ms.max(50));
    let explicit_yuki_work = path.components().any(|c| c.as_os_str() == "yuki-work");
    let mut last_mtimes: Option<HashMap<PathBuf, SystemTime>> = None;

    loop {
        let mut current_mtimes = HashMap::new();
        get_file_mtimes(path, &mut current_mtimes, explicit_yuki_work);

        let changed = match &last_mtimes {
            None => true,
            Some(prev) => prev != &current_mtimes,
        };

        if changed {
            if !json_mode {
                print!("\x1B[2J\x1B[H");
                let _ = stdout().flush();
            }

            run_check(path, json_mode);
            last_mtimes = Some(current_mtimes);
        }

        sleep(delay);
    }
}

fn get_file_mtimes(path: &Path, mtimes: &mut HashMap<PathBuf, SystemTime>, explicit_yuki_work: bool) {
    if path.is_file() {
        if path.extension().is_some_and(|ext| ext == "ykn") {
            if let Ok(meta) = fs::metadata(path) {
                if let Ok(mtime) = meta.modified() {
                    mtimes.insert(path.to_path_buf(), mtime);
                }
            }
        }
        return;
    }

    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if name.starts_with('.') || name == "target" || name == "temp" {
                    continue;
                }
                if name == "yuki-work" && !explicit_yuki_work {
                    continue;
                }
                get_file_mtimes(&p, mtimes, explicit_yuki_work);
            } else if p.extension().is_some_and(|ext| ext == "ykn") {
                if let Ok(meta) = fs::metadata(&p) {
                    if let Ok(mtime) = meta.modified() {
                        mtimes.insert(p, mtime);
                    }
                }
            }
        }
    }
}
