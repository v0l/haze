use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use haze_core::{Histogram, Step};

const DAY_MS: i64 = 86_400_000;
const LOST: u32 = u32::MAX;
const FIXED: usize = 8 + 4 + 2 + 2 + 2;
const EXTENSION: &str = "haze";

pub struct Store {
    root: PathBuf,
    open: HashMap<(String, i64), File>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bucket {
    pub start_ms: i64,
    pub steps: u32,
    pub sent: u64,
    pub received: u64,
    pub duplicates: u64,
    pub histogram: Histogram,
}

impl Bucket {
    pub fn loss(&self) -> f64 {
        if self.sent == 0 {
            0.0
        } else {
            1.0 - self.received as f64 / self.sent as f64
        }
    }
}

pub fn day_of(ms: i64) -> i64 {
    ms.div_euclid(DAY_MS)
}

pub fn date_name(day: i64) -> String {
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn day_from_name(name: &str) -> Option<i64> {
    let mut parts = name.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (parts.next()??, parts.next()??, parts.next()??);
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

pub fn encode(step: &Step) -> Vec<u8> {
    let mut payload = Vec::with_capacity(FIXED + step.rtts.len() * 4);
    payload.extend_from_slice(&step.start_ms.to_le_bytes());
    payload.extend_from_slice(&step.step_ms.to_le_bytes());
    payload.extend_from_slice(&(step.rtts.len() as u16).to_le_bytes());
    payload.extend_from_slice(&(step.duplicates.min(u16::MAX as u32) as u16).to_le_bytes());
    payload.extend_from_slice(&(step.reordered.min(u16::MAX as u32) as u16).to_le_bytes());
    for rtt in &step.rtts {
        let us = rtt
            .map(|r| r.as_micros().min(LOST as u128 - 1) as u32)
            .unwrap_or(LOST);
        payload.extend_from_slice(&us.to_le_bytes());
    }
    let mut record = Vec::with_capacity(8 + payload.len());
    record.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    record.extend_from_slice(&crc32fast::hash(&payload).to_le_bytes());
    record.extend_from_slice(&payload);
    record
}

fn decode(payload: &[u8]) -> Option<Step> {
    if payload.len() < FIXED {
        return None;
    }
    let u16_at = |i: usize| u16::from_le_bytes([payload[i], payload[i + 1]]);
    let sent = u16_at(12) as usize;
    if payload.len() != FIXED + sent * 4 {
        return None;
    }
    let rtts = payload[FIXED..]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| {
            let us = u32::from_le_bytes(*c);
            (us != LOST).then(|| Duration::from_micros(us as u64))
        })
        .collect();
    Some(Step {
        start_ms: i64::from_le_bytes(payload[0..8].try_into().unwrap()),
        step_ms: u32::from_le_bytes(payload[8..12].try_into().unwrap()),
        rtts,
        duplicates: u16_at(14) as u32,
        reordered: u16_at(16) as u32,
    })
}

pub fn read_records(bytes: &[u8]) -> (Vec<Step>, usize) {
    let mut steps = Vec::new();
    let mut at = 0;
    while at + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let crc = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap());
        let Some(payload) = bytes.get(at + 8..at + 8 + len) else {
            break;
        };
        if crc32fast::hash(payload) != crc {
            break;
        }
        let Some(step) = decode(payload) else {
            break;
        };
        steps.push(step);
        at += 8 + len;
    }
    (steps, at)
}

pub fn check_path(target: &str) -> io::Result<()> {
    let bad = target.is_empty()
        || target.split('/').any(|part| {
            part.is_empty() || part == "." || part == ".." || part.contains(['\\', '\0'])
        });
    if bad {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("`{target}` is not a target path"),
        ));
    }
    Ok(())
}

impl Store {
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Store> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Store {
            root,
            open: HashMap::new(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn day_file(&self, target: &str, day: i64) -> PathBuf {
        self.root
            .join(target)
            .join(format!("{}.{EXTENSION}", date_name(day)))
    }

    fn writer(&mut self, target: &str, day: i64) -> io::Result<&mut File> {
        check_path(target)?;
        let key = (target.to_string(), day);
        if !self.open.contains_key(&key) {
            self.open.retain(|(t, _), _| t != target);
            let path = self.day_file(target, day);
            fs::create_dir_all(path.parent().unwrap())?;
            let mut file = OpenOptions::new()
                .create(true)
                .read(true)
                .append(true)
                .open(&path)?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            let (_, valid) = read_records(&bytes);
            if valid < bytes.len() {
                file.set_len(valid as u64)?;
                file.seek(SeekFrom::End(0))?;
            }
            self.open.insert(key.clone(), file);
        }
        Ok(self.open.get_mut(&key).unwrap())
    }

    pub fn append(&mut self, target: &str, step: &Step) -> io::Result<()> {
        let record = encode(step);
        let file = self.writer(target, day_of(step.start_ms))?;
        let mut writer = BufWriter::new(file);
        writer.write_all(&record)?;
        writer.flush()?;
        Ok(())
    }

    pub fn sync(&mut self) -> io::Result<()> {
        for file in self.open.values() {
            file.sync_data()?;
        }
        Ok(())
    }

    pub fn targets(&self) -> io::Result<Vec<String>> {
        let mut found = Vec::new();
        collect_targets(&self.root, &self.root, &mut found)?;
        found.sort();
        Ok(found)
    }

    pub fn days(&self, target: &str) -> io::Result<Vec<i64>> {
        check_path(target)?;
        let dir = self.root.join(target);
        let mut days = Vec::new();
        let Ok(entries) = fs::read_dir(&dir) else {
            return Ok(days);
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == EXTENSION)
                && let Some(day) = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(day_from_name)
            {
                days.push(day);
            }
        }
        days.sort();
        Ok(days)
    }

    pub fn steps(&self, target: &str, from_ms: i64, to_ms: i64) -> io::Result<Vec<Step>> {
        let mut steps = Vec::new();
        for day in self.days(target)? {
            if day < day_of(from_ms) || day > day_of(to_ms - 1) {
                continue;
            }
            let bytes = fs::read(self.day_file(target, day))?;
            let (records, _) = read_records(&bytes);
            steps.extend(
                records
                    .into_iter()
                    .filter(|s| s.start_ms >= from_ms && s.start_ms < to_ms),
            );
        }
        steps.sort_by_key(|s| s.start_ms);
        Ok(steps)
    }

    pub fn query(
        &self,
        target: &str,
        from_ms: i64,
        to_ms: i64,
        width_ms: i64,
    ) -> io::Result<Vec<Bucket>> {
        let width = width_ms.max(1);
        let mut buckets: Vec<Bucket> = Vec::new();
        for step in self.steps(target, from_ms, to_ms)? {
            let start = from_ms + (step.start_ms - from_ms).div_euclid(width) * width;
            if buckets.last().is_none_or(|b| b.start_ms != start) {
                buckets.push(Bucket {
                    start_ms: start,
                    ..Bucket::default()
                });
            }
            let bucket = buckets.last_mut().unwrap();
            bucket.steps += 1;
            bucket.sent += step.sent() as u64;
            bucket.received += step.received() as u64;
            bucket.duplicates += step.duplicates as u64;
            bucket.histogram.merge(&step.histogram());
        }
        Ok(buckets)
    }

    pub fn prune(&mut self, keep_from_ms: i64) -> io::Result<usize> {
        let cutoff = day_of(keep_from_ms);
        self.open.retain(|(_, day), _| *day >= cutoff);
        let mut removed = 0;
        for target in self.targets()? {
            for day in self.days(&target)? {
                if day < cutoff {
                    fs::remove_file(self.day_file(&target, day))?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }
}

fn collect_targets(root: &Path, dir: &Path, found: &mut Vec<String>) -> io::Result<()> {
    let mut has_days = false;
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_targets(root, &path, found)?;
        } else if path.extension().is_some_and(|e| e == EXTENSION) {
            has_days = true;
        }
    }
    if has_days && let Ok(relative) = dir.strip_prefix(root) {
        let parts: Vec<String> = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        found.push(parts.join("/"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip() {
        assert_eq!(date_name(0), "1970-01-01");
        assert_eq!(date_name(day_of(1_700_000_000_000)), "2023-11-14");
        assert_eq!(date_name(day_of(951_782_400_000)), "2000-02-29");
        for day in [-1, 0, 59, 11_016, 19_675, 20_000, 30_000] {
            assert_eq!(day_from_name(&date_name(day)), Some(day));
        }
    }

    #[test]
    fn paths_cannot_escape_the_store() {
        for bad in ["", "../x", "a/../../b", "/abs", "a//b", "a/."] {
            assert!(check_path(bad).is_err(), "{bad}");
        }
        assert!(check_path("dns/cloudflare").is_ok());
    }
}
