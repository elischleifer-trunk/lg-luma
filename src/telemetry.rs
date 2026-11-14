//! What Luma tells a paired Home Assistant about itself: a bounded, in-memory log of what went
//! wrong (and a few things that went right), plus a periodic performance summary.
//!
//! Every entry gets a sequence number that starts at 1 when Luma starts, and Luma's start gets a
//! random `boot` ID, so a reader's cursor `boot:seq` says exactly where it left off. Home
//! Assistant reads the log live over `GET /api/events` (server-sent events, see [`stream`]): it
//! first gets what it missed since its cursor, then each entry as it is recorded. Only the newest
//! entries are kept; a reader that fell further behind is told how far ([`Missed`]).
//!
//! The log macros also print to stderr, as Luma always has.
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::Write,
    net::TcpStream,
    path::{Path, PathBuf},
    sync::{
        Condvar, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Log entries kept for readers that reconnect.
const MAX_LOG: usize = 500;
/// Performance summaries kept: at least two hours of them.
const MAX_METRICS: usize = 120;
const MAX_MESSAGE: usize = 1000;
/// Open event streams, one per paired server at most.
pub const MAX_STREAMS: usize = 8;
/// A comment line keeps idle streams alive and finds dead ones.
const HEARTBEAT: Duration = Duration::from_secs(15);
/// How often a summary is recorded while Luma draws, and while it is idle.
const METRICS_ACTIVE: Duration = Duration::from_secs(60);
const METRICS_IDLE: Duration = Duration::from_secs(10 * 60);
/// A frame that took this long after the previous one missed at least one 60 Hz refresh.
const DROPPED_MS: f64 = 25.;
/// Longer gaps are idle time between animations, not slow frames.
const IDLE_GAP_MS: f64 = 250.;
/// Frame work samples kept per summary.
const MAX_SAMPLES: usize = 7200;
const PANIC_FILE: &str = "luma-panic.txt";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Entry {
    pub seq: u64,
    /// Unix time in milliseconds.
    pub time: u64,
    pub level: Level,
    /// The part of Luma it came from: `cameras`, `overlay`, `inventory`…
    pub area: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl Entry {
    /// The server-sent event name: performance summaries are `metrics`, everything else `log`.
    pub fn kind(&self) -> &'static str {
        if self.area == "metrics" {
            "metrics"
        } else {
            "log"
        }
    }
}

/// Where a reader left off: entries after `seq` of the run `boot`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub boot: String,
    pub seq: u64,
}

impl Cursor {
    /// `boot:seq`, as sent in each event's `id`; anything else starts from the beginning.
    pub fn parse(text: &str) -> Self {
        text.split_once(':')
            .and_then(|(boot, seq)| {
                (!boot.is_empty()
                    && boot.len() <= 32
                    && boot.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| {
                    Some(Self {
                        boot: boot.to_owned(),
                        seq: seq.parse().ok()?,
                    })
                })
                .flatten()
            })
            .unwrap_or_default()
    }
}

/// Entries the reader asked for that were already dropped, up to and including this sequence
/// number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Missed(pub u64);

/// The kept entries; logs and summaries are bounded separately so summaries never push
/// failures out.
#[derive(Default)]
pub struct Ring {
    next: u64,
    logs: VecDeque<Entry>,
    metrics: VecDeque<Entry>,
    /// The highest sequence number dropped so far.
    dropped: u64,
}

impl Ring {
    pub fn push(
        &mut self,
        level: Level,
        area: &'static str,
        message: String,
        data: Option<Value>,
        time: u64,
    ) -> u64 {
        self.next += 1;
        let mut message = message;
        if message.len() > MAX_MESSAGE {
            let mut end = MAX_MESSAGE;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
            message.push('…');
        }
        let entry = Entry {
            seq: self.next,
            time,
            level,
            area,
            message,
            data,
        };
        let (list, max) = if entry.kind() == "metrics" {
            (&mut self.metrics, MAX_METRICS)
        } else {
            (&mut self.logs, MAX_LOG)
        };
        list.push_back(entry);
        if list.len() > max
            && let Some(old) = list.pop_front()
        {
            self.dropped = self.dropped.max(old.seq);
        }
        self.next
    }

    pub fn latest(&self) -> u64 {
        self.next
    }

    /// Entries after `after`, oldest first, and whether some of those were already dropped.
    pub fn since(&self, after: u64) -> (Vec<Entry>, Option<Missed>) {
        let mut entries: Vec<Entry> = self
            .logs
            .iter()
            .chain(&self.metrics)
            .filter(|entry| entry.seq > after)
            .cloned()
            .collect();
        entries.sort_by_key(|entry| entry.seq);
        let missed = (after < self.dropped).then_some(Missed(self.dropped));
        (entries, missed)
    }

    pub fn latest_metrics(&self) -> Option<&Entry> {
        self.metrics.back()
    }
}

struct Log {
    boot: String,
    started: Instant,
    ring: Mutex<Ring>,
    changed: Condvar,
}

fn log() -> &'static Log {
    static LOG: OnceLock<Log> = OnceLock::new();
    LOG.get_or_init(|| {
        let mut bytes = [0u8; 8];
        let _ = getrandom::getrandom(&mut bytes);
        Log {
            boot: bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
            started: Instant::now(),
            ring: Mutex::default(),
            changed: Condvar::new(),
        }
    })
}

fn ring() -> MutexGuard<'static, Ring> {
    log()
        .ring
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Records an entry and wakes the open streams. Prefer the `log_*!` macros.
pub fn record(level: Level, area: &'static str, message: String) {
    eprintln!("Luma {area}: {message}");
    push(level, area, message, None);
}

fn push(level: Level, area: &'static str, message: String, data: Option<Value>) {
    ring().push(level, area, message, data, unix_ms());
    log().changed.notify_all();
}

macro_rules! log_info {
    ($area:expr, $($arg:tt)*) => {
        $crate::telemetry::record($crate::telemetry::Level::Info, $area, format!($($arg)*))
    };
}
macro_rules! log_warn {
    ($area:expr, $($arg:tt)*) => {
        $crate::telemetry::record($crate::telemetry::Level::Warning, $area, format!($($arg)*))
    };
}
macro_rules! log_error {
    ($area:expr, $($arg:tt)*) => {
        $crate::telemetry::record($crate::telemetry::Level::Error, $area, format!($($arg)*))
    };
}
pub(crate) use {log_error, log_info, log_warn};

pub fn boot() -> &'static str {
    &log().boot
}

pub fn uptime() -> Duration {
    log().started.elapsed()
}

/// Everything kept, for `GET /api/diagnostics`.
pub fn snapshot() -> Value {
    let ring = ring();
    let (entries, _) = ring.since(0);
    json!({
        "boot": boot(),
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_s": uptime().as_secs(),
        "latest": ring.latest(),
        "metrics": ring.latest_metrics().and_then(|entry| entry.data.clone()),
        "entries": entries,
    })
}

/// Starts the performance summaries, reports a panic from Luma's previous run, and records
/// panics from now on. `directory` holds Luma's preferences.
pub fn start(directory: Option<&Path>) {
    let panic_file = directory.map(|directory| directory.join(PANIC_FILE));
    if let Some(path) = &panic_file
        && let Ok(previous) = std::fs::read_to_string(path)
    {
        let _ = std::fs::remove_file(path);
        log_error!("crash", "Luma's previous run stopped: {}", previous.trim());
    }
    install_panic_hook(panic_file);
    log_info!("lifecycle", "Luma {} started", env!("CARGO_PKG_VERSION"));
    let _ = thread::Builder::new()
        .name("telemetry".into())
        .spawn(summaries);
}

fn install_panic_hook(file: Option<PathBuf>) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = thread::current();
        let message = format!("panic in {}: {info}", thread.name().unwrap_or("a thread"));
        // Never wait for the log: the panic may have happened while it was locked.
        if let Ok(mut ring) = log().ring.try_lock() {
            ring.push(Level::Error, "crash", message.clone(), None, unix_ms());
            drop(ring);
            log().changed.notify_all();
        }
        // A panic on the main thread ends Luma, and with it the stream; the next start reports it.
        if let Some(path) = &file {
            let _ = std::fs::write(path, &message);
        }
        previous(info);
    }));
}

#[derive(Default)]
struct Frames {
    last: Option<Instant>,
    presented: u64,
    dropped: u64,
    /// CPU milliseconds spent composing, drawing and swapping each frame.
    work: Vec<f32>,
}

fn frames() -> MutexGuard<'static, Frames> {
    static FRAMES: OnceLock<Mutex<Frames>> = OnceLock::new();
    FRAMES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Counts a presented frame that took `work_ms` of CPU time to compose, draw and swap.
pub fn frame(presented: Instant, work_ms: f64) {
    let mut frames = frames();
    if let Some(last) = frames.last {
        let interval = presented.duration_since(last).as_secs_f64() * 1000.;
        if (DROPPED_MS..IDLE_GAP_MS).contains(&interval) {
            frames.dropped += 1;
        }
    }
    frames.last = Some(presented);
    frames.presented += 1;
    if frames.work.len() < MAX_SAMPLES {
        frames.work.push(work_ms as f32);
    }
}

/// Luma was hidden: the next frame's interval is not a slow frame.
pub fn frames_paused() {
    frames().last = None;
}

/// The frame summary since the last one, emptied.
fn take_frames() -> Value {
    let mut frames = frames();
    let mut work = std::mem::take(&mut frames.work);
    let summary = frame_summary(frames.presented, frames.dropped, &mut work);
    frames.presented = 0;
    frames.dropped = 0;
    summary
}

fn frame_summary(presented: u64, dropped: u64, work: &mut [f32]) -> Value {
    work.sort_by(f32::total_cmp);
    let percentile = |p: f64| -> Option<f64> {
        let index = ((work.len() as f64 - 1.) * p).round() as usize;
        work.get(index)
            .map(|ms| (f64::from(*ms) * 100.).round() / 100.)
    };
    json!({
        "presented": presented,
        "dropped": dropped,
        "work_p50_ms": percentile(0.5),
        "work_p95_ms": percentile(0.95),
        "work_max_ms": work.last().map(|ms| (f64::from(*ms) * 100.).round() / 100.),
    })
}

/// Resident memory in MiB and CPU ticks used so far, from `/proc` (Linux and webOS only).
fn process() -> (Option<f64>, Option<u64>) {
    let rss = std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|statm| statm.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map(|pages| (pages * 4096) as f64 / 1_048_576.);
    let ticks = std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|stat| {
            // Fields after the parenthesised command name; utime and stime are the 12th and 13th.
            let mut fields = stat.rsplit_once(')')?.1.split_whitespace().skip(11);
            Some(fields.next()?.parse::<u64>().ok()? + fields.next()?.parse::<u64>().ok()?)
        });
    (rss.map(|mb| (mb * 10.).round() / 10.), ticks)
}

fn summaries() {
    let mut last = (Instant::now(), process().1);
    loop {
        thread::sleep(METRICS_ACTIVE);
        let drawing = frames().presented > 0;
        if !drawing && last.0.elapsed() < METRICS_IDLE {
            continue;
        }
        let now = Instant::now();
        let (rss, ticks) = process();
        // Linux reports CPU time in 1/100 s ticks.
        let cpu = match (last.1, ticks) {
            (Some(before), Some(after)) => {
                let seconds = now.duration_since(last.0).as_secs_f64();
                Some(((after.saturating_sub(before) as f64 / seconds) * 10.).round() / 10.)
            }
            _ => None,
        };
        last = (now, ticks);
        let mut data = take_frames();
        data["rss_mb"] = json!(rss);
        data["cpu_percent"] = json!(cpu);
        data["uptime_s"] = json!(uptime().as_secs());
        let message = format!(
            "{} frames, p95 {} ms, {} dropped, {} MiB",
            data["presented"], data["work_p95_ms"], data["dropped"], data["rss_mb"]
        );
        push(Level::Info, "metrics", message, Some(data));
    }
}

static STREAMS: AtomicUsize = AtomicUsize::new(0);

/// Whether another stream may open; the caller then hands its connection to [`stream`].
pub fn stream_available() -> bool {
    STREAMS.load(Ordering::Acquire) < MAX_STREAMS
}

/// One server-sent event.
pub fn event(entry: &Entry) -> String {
    let data = serde_json::to_string(entry).unwrap_or_default();
    format!(
        "id: {}:{}\nevent: {}\ndata: {data}\n\n",
        boot(),
        entry.seq,
        entry.kind()
    )
}

/// Serves `GET /api/events` on its own thread until the reader goes away or `alive` turns false
/// (the server was unpaired or Home Assistant turned off in Luma).
pub fn stream(mut connection: TcpStream, from: Cursor, alive: impl Fn() -> bool + Send + 'static) {
    STREAMS.fetch_add(1, Ordering::AcqRel);
    let spawned = thread::Builder::new().name("events".into()).spawn(move || {
        let _ = serve_stream(&mut connection, &from, &alive);
        STREAMS.fetch_sub(1, Ordering::AcqRel);
    });
    if spawned.is_err() {
        STREAMS.fetch_sub(1, Ordering::AcqRel);
    }
}

fn serve_stream(
    connection: &mut TcpStream,
    from: &Cursor,
    alive: &dyn Fn() -> bool,
) -> std::io::Result<()> {
    connection.set_nonblocking(false)?;
    connection.set_write_timeout(Some(Duration::from_secs(10)))?;
    // A run that restarted numbers from 1 again: the reader gets everything kept.
    let restarted = from.boot != boot();
    let mut after = if restarted { 0 } else { from.seq };
    let hello = json!({
        "boot": boot(),
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_s": uptime().as_secs(),
        "restarted": restarted && !from.boot.is_empty(),
        "latest": ring().latest(),
    });
    write!(
        connection,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\nX-Accel-Buffering: no\r\n\r\nretry: 5000\n\nevent: hello\ndata: {hello}\n\n"
    )?;
    loop {
        let (entries, missed) = ring().since(after);
        let mut out = String::new();
        if let Some(Missed(through)) = missed {
            out.push_str(&format!(
                "event: missed\ndata: {}\n\n",
                json!({"after": after, "through": through})
            ));
        }
        for entry in &entries {
            out.push_str(&event(entry));
            after = entry.seq;
        }
        if !out.is_empty() {
            connection.write_all(out.as_bytes())?;
        }
        if !alive() {
            return Ok(());
        }
        let ring = ring();
        let (ring, timeout) = log()
            .changed
            .wait_timeout_while(ring, HEARTBEAT, |ring| ring.latest() <= after)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        drop(ring);
        if timeout.timed_out() {
            connection.write_all(b": ping\n\n")?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ring: &mut Ring, area: &'static str, message: &str) -> u64 {
        ring.push(Level::Warning, area, message.into(), None, 0)
    }

    #[test]
    fn replays_after_a_cursor_in_order() {
        let mut ring = Ring::default();
        entry(&mut ring, "cameras", "one");
        ring.push(Level::Info, "metrics", "m".into(), Some(json!({})), 0);
        entry(&mut ring, "overlay", "three");
        let (entries, missed) = ring.since(1);
        assert_eq!(missed, None);
        assert_eq!(
            entries
                .iter()
                .map(|e| (e.seq, e.kind()))
                .collect::<Vec<_>>(),
            vec![(2, "metrics"), (3, "log")]
        );
        assert!(ring.since(3).0.is_empty());
    }

    #[test]
    fn reports_entries_that_were_dropped() {
        let mut ring = Ring::default();
        for index in 0..MAX_LOG + 3 {
            entry(&mut ring, "cameras", &index.to_string());
        }
        let (entries, missed) = ring.since(0);
        assert_eq!(entries.len(), MAX_LOG);
        assert_eq!(entries[0].seq, 4);
        assert_eq!(missed, Some(Missed(3)));
        assert_eq!(ring.since(3).1, None);
    }

    #[test]
    fn summaries_never_push_out_failures() {
        let mut ring = Ring::default();
        entry(&mut ring, "overlay", "failed");
        for _ in 0..MAX_METRICS * 2 {
            ring.push(Level::Info, "metrics", String::new(), Some(json!({})), 0);
        }
        let (entries, missed) = ring.since(0);
        assert_eq!(entries[0].message, "failed");
        assert_eq!(entries.len(), MAX_METRICS + 1);
        assert_eq!(missed, Some(Missed(MAX_METRICS as u64 + 1)));
    }

    #[test]
    fn long_messages_are_cut_on_a_character_boundary() {
        let mut ring = Ring::default();
        entry(&mut ring, "cameras", &"é".repeat(MAX_MESSAGE));
        let message = &ring.since(0).0[0].message;
        assert!(message.len() <= MAX_MESSAGE + '…'.len_utf8());
        assert!(message.ends_with('…'));
    }

    #[test]
    fn parses_cursors() {
        assert_eq!(
            Cursor::parse("0a1b:42"),
            Cursor {
                boot: "0a1b".into(),
                seq: 42
            }
        );
        assert_eq!(Cursor::parse("nothex:4"), Cursor::default());
        assert_eq!(Cursor::parse("0a1b:x"), Cursor::default());
        assert_eq!(Cursor::parse(""), Cursor::default());
    }

    #[test]
    fn formats_events() {
        let mut ring = Ring::default();
        entry(&mut ring, "cameras", "no HLS stream");
        let text = event(&ring.since(0).0[0]);
        assert!(text.starts_with(&format!("id: {}:1\nevent: log\ndata: {{", boot())));
        assert!(text.contains(r#""level":"warning","area":"cameras","message":"no HLS stream""#));
        assert!(text.ends_with("}\n\n"));
    }

    #[test]
    fn summarises_frames() {
        let mut work = vec![4., 2., 30., 3., 5.];
        let summary = frame_summary(5, 1, &mut work);
        assert_eq!(summary["presented"], 5);
        assert_eq!(summary["dropped"], 1);
        assert_eq!(summary["work_p50_ms"], 4.);
        assert_eq!(summary["work_p95_ms"], 30.);
        assert_eq!(summary["work_max_ms"], 30.);
        assert_eq!(frame_summary(0, 0, &mut [])["work_p95_ms"], Value::Null);
    }
}
