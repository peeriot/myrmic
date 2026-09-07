//! Live table views: `m cells` and `m network` refresh on the alternate screen
//! until interrupted, redrawing at frame rate so rows fade in and out between
//! refreshes.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::hash::Hash;
use std::io::{IsTerminal as _, Write as _};
use std::pin::pin;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};

use crate::render::{DIMMED, RESET};

/// How long a row stays coloured after arriving or departing.
pub const FADE: Duration = Duration::from_millis(1_500);

/// Time between redraws while live. Refreshes happen on `--interval`; this
/// only sets how smoothly the fades animate between them.
const FRAME: Duration = Duration::from_millis(100);

const ERROR: &str = "\x1b[1;31m";

/// Green, vivid to pale: a new row lands bright and settles into the table.
const ARRIVING: [&str; 6] = [
    "\x1b[1;38;5;46m",
    "\x1b[38;5;46m",
    "\x1b[38;5;77m",
    "\x1b[38;5;114m",
    "\x1b[38;5;151m",
    "\x1b[38;5;188m",
];

/// Struck-through red, vivid to grey: a departed row burns out before it goes.
const DEPARTING: [&str; 6] = [
    "\x1b[9;1;38;5;196m",
    "\x1b[9;38;5;196m",
    "\x1b[9;38;5;167m",
    "\x1b[9;38;5;138m",
    "\x1b[9;38;5;245m",
    "\x1b[9;38;5;240m",
];

#[derive(clap::Args, Clone, Copy, Debug)]
pub struct Opts {
    /// Print once and exit instead of refreshing until interrupted. Implied
    /// when stdout is not a terminal.
    #[clap(long)]
    pub once: bool,

    /// Time between refreshes while live, e.g. `500ms`, `2.5s`, `1m`.
    #[clap(long, default_value = "2.5s", value_name = "DURATION")]
    pub interval: humantime::Duration,
}

/// What a live command shows: how to fetch its state, fold a fetch in, and
/// render it. Fetching borrows the view immutably so frames keep drawing
/// while a query is in flight.
pub trait View {
    type Snapshot;

    fn fetch(&self) -> impl Future<Output = Result<Self::Snapshot>>;

    /// Folds a snapshot in, recording arrivals and departures as of `now`.
    fn apply(&mut self, snapshot: Self::Snapshot, now: Instant);

    fn draw(&self, now: Instant, styled: bool) -> String;

    /// Drops departing rows and ends every fade, for the final still frame.
    fn settle(&mut self);
}

/// Runs `view` once, or live until Ctrl+C. Live mode leaves the terminal as
/// it found it and prints the final frame, so the last state stays in the
/// scrollback.
pub async fn run<V: View>(opts: Opts, mut view: V) -> Result<()> {
    let styled = std::io::stdout().is_terminal();
    if opts.once || !styled {
        let snapshot = view.fetch().await?;
        let now = Instant::now();
        view.apply(snapshot, now);
        print!("{}", view.draw(now, styled));
        return Ok(());
    }

    let mut screen = Screen::enter()?;
    let mut header = Header {
        interval: opts.interval.into(),
        refreshed: None,
        error: None,
    };
    let mut ctrl_c = pin!(tokio::signal::ctrl_c());
    let mut frames = tokio::time::interval(FRAME);
    frames.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    'live: loop {
        let fetched = {
            let mut fetch = pin!(view.fetch());
            loop {
                tokio::select! {
                    _ = &mut ctrl_c => break 'live,
                    fetched = &mut fetch => break fetched,
                    _ = frames.tick() => screen.draw(&frame(&view, &header))?,
                }
            }
        };

        let now = Instant::now();
        match fetched {
            Ok(snapshot) => {
                view.apply(snapshot, now);
                header.refreshed = Some(now);
                header.error = None;
            }
            Err(err) => header.error = Some(crate::format_error(&err)),
        }

        let mut pause = pin!(tokio::time::sleep(header.interval));
        loop {
            tokio::select! {
                _ = &mut ctrl_c => break 'live,
                () = &mut pause => break,
                _ = frames.tick() => screen.draw(&frame(&view, &header))?,
            }
        }
    }

    drop(screen);
    if header.refreshed.is_none() {
        // Interrupted before anything was ever shown: nothing to leave behind
        // but the reason, if there was one.
        return header.error.map_or(Ok(()), |err| Err(anyhow::anyhow!(err)));
    }
    view.settle();
    print!("{}", view.draw(Instant::now(), true));
    Ok(())
}

/// The header followed by the table, once there has been an answer to show.
/// Until then an empty view would read as an empty network.
fn frame<V: View>(view: &V, header: &Header) -> String {
    let now = Instant::now();
    let mut out = header.render(now);
    if header.refreshed.is_some() {
        out.push_str(&view.draw(now, true));
    }
    out
}

struct Header {
    interval: Duration,
    refreshed: Option<Instant>,
    error: Option<String>,
}

impl Header {
    fn render(&self, now: Instant) -> String {
        let every = humantime::format_duration(self.interval);
        let refreshed = match self.refreshed {
            Some(at) => format!(
                "refreshed {}s ago",
                now.saturating_duration_since(at).as_secs()
            ),
            None => "waiting for the first answer".to_owned(),
        };
        let mut out = format!("{DIMMED}every {every} · {refreshed} · ctrl-c to quit{RESET}\n");
        if let Some(err) = &self.error {
            let flat = err.lines().collect::<Vec<_>>().join("; ");
            let _ = writeln!(out, "{ERROR}refresh failed: {flat}{RESET}");
        }
        out.push('\n');
        out
    }
}

/// The alternate screen for the lifetime of the live view. Dropping it hands
/// the terminal back exactly as it was.
struct Screen {
    last: Option<(usize, String)>,
}

impl Screen {
    /// Switches to the alternate screen with the cursor hidden and auto-wrap
    /// off, so an over-wide row is cut short instead of spilling onto the
    /// next line and pushing the table down.
    fn enter() -> Result<Self> {
        emit("\x1b[?1049h\x1b[?25l\x1b[?7l\x1b[H\x1b[2J")?;
        Ok(Self { last: None })
    }

    /// Redraws in place, skipping frames identical to the one on screen.
    fn draw(&mut self, frame: &str) -> Result<()> {
        let rows = terminal_rows();
        if self
            .last
            .as_ref()
            .is_some_and(|(r, f)| *r == rows && f == frame)
        {
            return Ok(());
        }
        emit(&compose(frame, rows))?;
        self.last = Some((rows, frame.to_owned()));
        Ok(())
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = emit("\x1b[?7h\x1b[?25h\x1b[?1049l");
    }
}

fn emit(bytes: &str) -> Result<()> {
    let mut out = std::io::stdout().lock();
    out.write_all(bytes.as_bytes())
        .and_then(|()| out.flush())
        .context("unable to write to the terminal")
}

/// The terminal bytes that put `frame` on screen from the top-left: every
/// line is erased to its end as it is written, and whatever the previous
/// frame left below is cleared, so nothing ever flashes blank in between. A
/// frame taller than the screen is cut, the last line saying by how much. The
/// last line gets no newline, which would scroll a full screen by one row.
fn compose(frame: &str, rows: usize) -> String {
    let lines: Vec<&str> = frame.lines().collect();
    let mut out = String::from("\x1b[H");
    if lines.len() <= rows {
        out.push_str(&lines.join("\x1b[K\r\n"));
    } else {
        let shown = rows.saturating_sub(1);
        for line in &lines[..shown] {
            out.push_str(line);
            out.push_str("\x1b[K\r\n");
        }
        let _ = write!(out, "{DIMMED}… {} more rows{RESET}", lines.len() - shown);
    }
    out.push_str("\x1b[K\x1b[J");
    out
}

fn terminal_rows() -> usize {
    // SAFETY: TIOCGWINSZ writes a `winsize` into the zeroed struct we own and
    // nothing else; on failure the struct stays zeroed and is ignored.
    let size = unsafe {
        let mut size: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &raw mut size) != 0 {
            size.ws_row = 0;
        }
        size
    };
    match size.ws_row {
        0 => 24,
        rows => usize::from(rows),
    }
}

/// How far a row is through its fade, in permille, or where it stands once
/// the fade is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Arriving(usize),
    Departing(usize),
    /// Present and past any fade.
    Settled,
    /// Never seen, or departed long enough ago to have faded out.
    Gone,
}

impl Phase {
    /// The colour to paint a row in this phase, if any.
    pub fn sgr(self) -> Option<&'static str> {
        fn pick(ramp: &[&'static str], permille: usize) -> &'static str {
            ramp[(permille * ramp.len() / 1_000).min(ramp.len() - 1)]
        }
        match self {
            Self::Arriving(p) => Some(pick(&ARRIVING, p)),
            Self::Departing(p) => Some(pick(&DEPARTING, p)),
            Self::Settled | Self::Gone => None,
        }
    }
}

/// `line` coloured with `sgr` end to end. The colour is re-asserted after
/// every reset the line already carries, since the id column's own
/// highlight ends in one.
pub fn paint(line: &str, sgr: &str) -> String {
    format!(
        "{sgr}{}{RESET}",
        line.replace(RESET, &format!("{RESET}{sgr}"))
    )
}

fn permille(elapsed: Duration, fade: Duration) -> usize {
    let p = elapsed.as_millis().saturating_mul(1_000) / fade.as_millis().max(1);
    usize::try_from(p).unwrap_or(1_000).min(1_000)
}

/// Which rows arrived or departed recently, keyed by row identity. A row
/// whose `version` changes (a cell's generation, say) counts as arriving
/// again. The first observation seeds the set without anything arriving.
pub struct Pops<K, V = ()> {
    fade: Duration,
    rows: HashMap<K, Seen<V>>,
    primed: bool,
}

struct Seen<V> {
    version: V,
    /// `None` once settled.
    arrived: Option<Instant>,
    departed: Option<Instant>,
}

impl<K: Hash + Eq, V: PartialEq> Pops<K, V> {
    pub fn new(fade: Duration) -> Self {
        Self {
            fade,
            rows: HashMap::new(),
            primed: false,
        }
    }

    /// Records what is present as of `now`: anything else starts departing,
    /// anything new — or back after departing, or under a new version —
    /// starts arriving. Rows that finished fading out are forgotten.
    pub fn observe(&mut self, now: Instant, present: impl IntoIterator<Item = (K, V)>) {
        for seen in self.rows.values_mut() {
            seen.departed.get_or_insert(now);
        }
        for (key, version) in present {
            let arrived = self.primed.then_some(now);
            match self.rows.get_mut(&key) {
                Some(seen) if seen.version == version && seen.departed == Some(now) => {
                    seen.departed = None;
                }
                Some(seen) => {
                    *seen = Seen {
                        version,
                        arrived,
                        departed: None,
                    };
                }
                None => {
                    self.rows.insert(
                        key,
                        Seen {
                            version,
                            arrived,
                            departed: None,
                        },
                    );
                }
            }
        }
        let fade = self.fade;
        self.rows
            .retain(|_, seen| seen.departed.is_none_or(|at| now.duration_since(at) < fade));
        self.primed = true;
    }

    pub fn phase(&self, key: &K, now: Instant) -> Phase {
        let Some(seen) = self.rows.get(key) else {
            return Phase::Gone;
        };
        if let Some(at) = seen.departed {
            return match permille(now.saturating_duration_since(at), self.fade) {
                1_000.. => Phase::Gone,
                p => Phase::Departing(p),
            };
        }
        match seen
            .arrived
            .map(|at| permille(now.saturating_duration_since(at), self.fade))
        {
            Some(p) if p < 1_000 => Phase::Arriving(p),
            _ => Phase::Settled,
        }
    }

    /// Keys absent from the last observation but still fading out.
    pub fn departing(&self, now: Instant) -> impl Iterator<Item = &K> {
        self.rows
            .keys()
            .filter(move |key| matches!(self.phase(key, now), Phase::Departing(_)))
    }

    /// Whether `key` was in the last observation.
    pub fn present(&self, key: &K) -> bool {
        self.rows
            .get(key)
            .is_some_and(|seen| seen.departed.is_none())
    }

    /// Ends every fade: departing rows go, arrivals settle.
    pub fn settle(&mut self) {
        self.rows.retain(|_, seen| seen.departed.is_none());
        for seen in self.rows.values_mut() {
            seen.arrived = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::ready;

    use super::*;

    const FADE: Duration = Duration::from_secs(1);

    fn pops() -> Pops<&'static str, u8> {
        Pops::new(FADE)
    }

    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    #[test]
    fn the_first_observation_settles_everything() {
        let t0 = Instant::now();
        let mut pops = pops();
        pops.observe(t0, [("a", 1), ("b", 1)]);

        assert_eq!(pops.phase(&"a", t0), Phase::Settled);
        assert_eq!(pops.phase(&"b", at(t0, 10)), Phase::Settled);
        assert_eq!(pops.phase(&"c", t0), Phase::Gone);
    }

    #[test]
    fn a_new_key_fades_in() {
        let t0 = Instant::now();
        let mut pops = pops();
        pops.observe(t0, [("a", 1)]);
        pops.observe(at(t0, 100), [("a", 1), ("b", 1)]);

        assert_eq!(pops.phase(&"a", at(t0, 100)), Phase::Settled);
        assert_eq!(pops.phase(&"b", at(t0, 100)), Phase::Arriving(0));
        assert_eq!(pops.phase(&"b", at(t0, 600)), Phase::Arriving(500));
        assert_eq!(pops.phase(&"b", at(t0, 1_100)), Phase::Settled);
        assert!(pops.present(&"b"));
    }

    #[test]
    fn a_changed_version_fades_in_again() {
        let t0 = Instant::now();
        let mut pops = pops();
        pops.observe(t0, [("a", 1)]);
        pops.observe(at(t0, 100), [("a", 2)]);

        assert_eq!(pops.phase(&"a", at(t0, 100)), Phase::Arriving(0));
    }

    #[test]
    fn a_missing_key_fades_out_then_goes() {
        let t0 = Instant::now();
        let mut pops = pops();
        pops.observe(t0, [("a", 1), ("b", 1)]);
        pops.observe(at(t0, 100), [("a", 1)]);

        assert_eq!(pops.phase(&"b", at(t0, 100)), Phase::Departing(0));
        assert_eq!(pops.phase(&"b", at(t0, 350)), Phase::Departing(250));
        assert_eq!(pops.departing(at(t0, 350)).collect::<Vec<_>>(), [&"b"]);
        assert!(!pops.present(&"b"));

        assert_eq!(pops.phase(&"b", at(t0, 1_100)), Phase::Gone);
        assert_eq!(pops.departing(at(t0, 1_100)).count(), 0);

        // Forgotten on the next observation, so a later return is a fresh arrival.
        pops.observe(at(t0, 1_100), [("a", 1)]);
        pops.observe(at(t0, 1_200), [("a", 1), ("b", 1)]);
        assert_eq!(pops.phase(&"b", at(t0, 1_200)), Phase::Arriving(0));
    }

    #[test]
    fn a_key_back_before_its_fade_ends_arrives_again() {
        let t0 = Instant::now();
        let mut pops = pops();
        pops.observe(t0, [("a", 1), ("b", 1)]);
        pops.observe(at(t0, 100), [("a", 1)]);
        pops.observe(at(t0, 500), [("a", 1), ("b", 1)]);

        assert_eq!(pops.phase(&"b", at(t0, 500)), Phase::Arriving(0));
    }

    #[test]
    fn settle_drops_departing_rows_and_ends_fades() {
        let t0 = Instant::now();
        let mut pops = pops();
        pops.observe(t0, [("a", 1), ("b", 1)]);
        pops.observe(at(t0, 100), [("a", 1), ("c", 1)]);
        pops.settle();

        assert_eq!(pops.phase(&"a", at(t0, 100)), Phase::Settled);
        assert_eq!(pops.phase(&"b", at(t0, 100)), Phase::Gone);
        assert_eq!(pops.phase(&"c", at(t0, 100)), Phase::Settled);
        assert!(!pops.present(&"b"));
        assert!(pops.present(&"c"));
    }

    #[test]
    fn ramps_run_from_vivid_to_faint() {
        assert_eq!(Phase::Arriving(0).sgr(), Some(ARRIVING[0]));
        assert_eq!(Phase::Arriving(166).sgr(), Some(ARRIVING[0]));
        assert_eq!(Phase::Arriving(167).sgr(), Some(ARRIVING[1]));
        assert_eq!(Phase::Arriving(999).sgr(), Some(ARRIVING[5]));
        assert_eq!(Phase::Departing(500).sgr(), Some(DEPARTING[3]));
        assert_eq!(Phase::Departing(1_000).sgr(), Some(DEPARTING[5]));
        assert_eq!(Phase::Settled.sgr(), None);
        assert_eq!(Phase::Gone.sgr(), None);
    }

    #[test]
    fn paint_reasserts_the_colour_after_inner_resets() {
        let line = format!("  a  {RESET}b{RESET}  c");

        assert_eq!(
            paint(&line, "<g>"),
            format!("<g>  a  {RESET}<g>b{RESET}<g>  c{RESET}")
        );
    }

    #[test]
    fn compose_erases_each_line_and_never_scrolls_a_full_screen() {
        assert_eq!(
            compose("one\ntwo\n", 2),
            "\x1b[Hone\x1b[K\r\ntwo\x1b[K\x1b[J"
        );
    }

    #[test]
    fn compose_cuts_a_tall_frame_and_says_so() {
        assert_eq!(
            compose("1\n2\n3\n4\n5\n", 3),
            format!("\x1b[H1\x1b[K\r\n2\x1b[K\r\n{DIMMED}… 3 more rows{RESET}\x1b[K\x1b[J")
        );
    }

    struct Fake;

    impl View for Fake {
        type Snapshot = ();

        fn fetch(&self) -> impl Future<Output = Result<()>> {
            ready(Ok(()))
        }

        fn apply(&mut self, (): (), _: Instant) {}

        fn draw(&self, _: Instant, _: bool) -> String {
            "the table\n".to_owned()
        }

        fn settle(&mut self) {}
    }

    #[test]
    fn the_table_is_drawn_only_after_the_first_answer() {
        let t0 = Instant::now();
        let mut header = Header {
            interval: Duration::from_secs(1),
            refreshed: None,
            error: Some("no reply".to_owned()),
        };
        assert!(!frame(&Fake, &header).contains("the table"));

        header.refreshed = Some(t0);
        assert!(frame(&Fake, &header).ends_with("the table\n"));
    }

    #[test]
    fn the_header_reports_the_last_refresh_and_any_error() {
        let t0 = Instant::now();
        let mut header = Header {
            interval: Duration::from_millis(2_500),
            refreshed: None,
            error: None,
        };
        assert_eq!(
            header.render(t0),
            format!(
                "{DIMMED}every 2s 500ms · waiting for the first answer · ctrl-c to quit{RESET}\n\n"
            )
        );

        header.refreshed = Some(t0);
        header.error = Some("no reply\nCaused by: timeout".to_owned());
        assert_eq!(
            header.render(at(t0, 3_200)),
            format!(
                "{DIMMED}every 2s 500ms · refreshed 3s ago · ctrl-c to quit{RESET}\n\
                 {ERROR}refresh failed: no reply; Caused by: timeout{RESET}\n\n"
            )
        );
    }
}
