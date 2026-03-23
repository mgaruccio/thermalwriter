// MangoHudProvider: reads MangoHud CSV logs for FPS, frametime, load metrics.
// MangoHud writes CSV files to ~/.local/share/MangoHud/ (or $MANGOHUD_LOG_DIR).
// We read only the header line + last data line — NOT the full file (can be 100s of MB).

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use anyhow::Result;

use super::{SensorDescriptor, SensorProvider, SensorReading};

pub struct MangoHudProvider {
    log_dir: PathBuf,
}

impl MangoHudProvider {
    pub fn new() -> Self {
        let log_dir = if let Ok(dir) = std::env::var("MANGOHUD_LOG_DIR") {
            PathBuf::from(dir)
        } else {
            dirs_next_mangohud_default()
        };
        Self { log_dir }
    }

    /// For testing with a custom log directory.
    pub fn with_log_dir(log_dir: PathBuf) -> Self {
        Self { log_dir }
    }

    /// Find the most recently modified .csv file in the log directory.
    fn find_latest_csv(&self) -> Option<PathBuf> {
        let entries = fs::read_dir(&self.log_dir).ok()?;
        entries
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("csv"))
            .filter_map(|e| {
                let mtime = e.metadata().ok()?.modified().ok()?;
                Some((e.path(), mtime))
            })
            .max_by_key(|(_, mtime)| *mtime)
            .map(|(path, _)| path)
    }

    /// Read only the header line and last data line from a potentially huge CSV.
    /// Reads the header from the start, then seeks to near the end to find the last line.
    /// Never loads the full file — only reads up to 4KB from the tail.
    fn read_header_and_last_line(path: &std::path::Path) -> Option<(String, String)> {
        use std::io::Read;

        // Pass 1: read the header line from the start
        let file = File::open(path).ok()?;
        let mut reader = BufReader::new(file);
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end_matches('\n').trim_end_matches('\r').to_string();
        if header.is_empty() {
            return None;
        }

        // Pass 2: read only the last 4KB of the file to find the last data line
        let file2 = File::open(path).ok()?;
        let file_len = file2.metadata().ok()?.len();
        if file_len == 0 {
            return None;
        }

        let scan_size = file_len.min(4096) as usize;
        let scan_start = file_len - scan_size as u64;

        let mut reader2 = BufReader::new(file2);
        reader2.seek(SeekFrom::Start(scan_start)).ok()?;

        let mut tail_bytes = vec![0u8; scan_size];
        let n = reader2.read(&mut tail_bytes).ok()?;
        let tail_str = String::from_utf8_lossy(&tail_bytes[..n]);

        // Find the last non-empty line in the tail
        let last_line = tail_str
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .map(|l| l.trim().to_string())?;

        // If only the header exists in the file, last_line will equal it
        if last_line == header.trim() {
            return None;
        }

        Some((header, last_line))
    }
}

fn dirs_next_mangohud_default() -> PathBuf {
    // ~/.local/share/MangoHud/
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local/share/MangoHud")
    } else {
        PathBuf::from("/tmp")
    }
}

/// Keys we extract from MangoHud CSV (others are ignored).
const WANTED_KEYS: &[&str] = &["fps", "frametime", "cpu_load", "gpu_load"];

impl SensorProvider for MangoHudProvider {
    fn name(&self) -> &str {
        "mangohud"
    }

    fn poll(&mut self) -> Result<Vec<SensorReading>> {
        let mut readings = Vec::new();

        let csv_path = match self.find_latest_csv() {
            Some(p) => p,
            None => return Ok(readings), // No CSV files — return empty
        };

        let (header, last_line) = match Self::read_header_and_last_line(&csv_path) {
            Some(pair) => pair,
            None => return Ok(readings), // Headers only or empty file
        };

        let headers: Vec<&str> = header.split(',').collect();
        let values: Vec<&str> = last_line.split(',').collect();

        if headers.len() != values.len() {
            return Ok(readings); // Malformed CSV row
        }

        for (col, val) in headers.iter().zip(values.iter()) {
            let key = col.trim();
            let raw = val.trim();

            if !WANTED_KEYS.contains(&key) {
                continue;
            }

            let (formatted, unit) = match key {
                "fps" => {
                    // Round to integer
                    let f: f64 = match raw.parse() { Ok(v) => v, Err(_) => continue };
                    (format!("{}", f.round() as i64), "fps".to_string())
                }
                "frametime" => {
                    // 1 decimal place
                    let f: f64 = match raw.parse() { Ok(v) => v, Err(_) => continue };
                    (format!("{:.1}", f), "ms".to_string())
                }
                "cpu_load" | "gpu_load" => {
                    // Integer percent
                    let f: f64 = match raw.parse() { Ok(v) => v, Err(_) => continue };
                    (format!("{}", f.round() as i64), "%".to_string())
                }
                _ => continue,
            };

            readings.push(SensorReading {
                key: key.to_string(),
                value: formatted,
                unit,
            });
        }

        Ok(readings)
    }

    fn available_sensors(&self) -> Vec<SensorDescriptor> {
        WANTED_KEYS.iter().map(|k| SensorDescriptor {
            key: k.to_string(),
            name: k.to_string(),
            unit: String::new(),
        }).collect()
    }
}
