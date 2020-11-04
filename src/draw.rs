use std::fmt::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use crate::{BarBuild, BarDraw, BarDrawParams, BarState};

struct BoringBarState {
    total: AtomicUsize,
    current: AtomicUsize,
    finished: AtomicBool,
    aborted: AtomicBool,
}

/// A handle to the boring bar that can used to update the progress of the bar.
#[derive(Clone)]
pub struct BoringBarHandle {
    state: Arc<BoringBarState>,
    multibar: BarState,
}

/// The handle used to update the progress bar.
impl BoringBarHandle {
    /// Set a new value for the progress and trigger a redraw.
    pub fn set(&self, val: usize) {
        self.state.current.store(val, Ordering::Release);
        self.multibar.redraw()
    }

    /// Increment the progress by the given amount.
    pub fn incr(&self, incr: usize) {
        self.state.current.fetch_add(incr, Ordering::Release);
        self.multibar.redraw()
    }

    /// Increment the progress by 1.
    pub fn tick(&self) {
        self.incr(1)
    }

    /// Finish rendering the bar. It will be redrawn once more and never again.
    pub fn finish(&self) {
        self.state.finished.store(true, Ordering::Release);
        self.multibar.redraw()
    }
}

/// A builder for the boring bar. Right now you can only specify a name and a total. Use a total
/// of `0` for a looping progress bar.
pub struct BoringBarBuilder {
    name: String,
    total: usize,
}

impl BoringBarBuilder {
    /// A progress bar with the total size. If the total is 0, progress will be shown as a rolling
    /// update.
    pub fn new(name: String, total: usize) -> BoringBarBuilder {
        BoringBarBuilder { name, total }
    }
}

pub const RED: &str = "\u{1b}[49;31m";
pub const GREEN: &str = "\u{1b}[49;32m";
pub const YELLOW: &str = "\u{1b}[49;33m";
pub const BLUE: &str = "\u{1b}[49;34m";
pub const CLEAR: &str = "\u{1b}[0m";

impl BarBuild for BoringBarBuilder {
    type Handle = BoringBarHandle;
    type Drawer = BoringBarDrawer;

    fn build(self, multibar: BarState) -> (Self::Handle, Self::Drawer) {
        let state = Arc::new(BoringBarState {
            total: AtomicUsize::new(self.total),
            current: AtomicUsize::new(0),
            finished: AtomicBool::new(false),
            aborted: AtomicBool::new(false),
        });
        let handle = BoringBarHandle {
            state: state.clone(),
            multibar,
        };
        let drawer = BoringBarDrawer {
            name: self.name,
            draws: 0,
            state,
        };
        (handle, drawer)
    }
}

/// This is the `BoringBarDrawer`.
pub struct BoringBarDrawer {
    name: String,
    draws: usize,
    state: Arc<BoringBarState>,
}

impl BarDraw for BoringBarDrawer {
    fn is_finished(&self) -> bool {
        self.state.finished.load(Ordering::Acquire)
    }

    fn draw(&mut self, params: &BarDrawParams) -> String {
        let total = self.state.total.load(Ordering::Acquire);
        let current = self.state.current.load(Ordering::Acquire);
        let was_finished = self
            .state
            .finished
            .fetch_or(params.finished(), Ordering::Acquire);
        let was_aborted = self
            .state
            .aborted
            .fetch_or(params.aborted(), Ordering::Acquire);
        let finished = was_finished || params.finished();
        let aborted = was_aborted || params.aborted();
        let len = 60;

        let mut buffer = String::new();
        if total != 0 {
            let used = std::cmp::min(
                len,
                (len as f64 * (current as f64 / total as f64)).round() as usize,
            );
            let remaining = len - used;
            let bar_colour = if aborted {
                RED
            } else if finished {
                if remaining == 0 {
                    GREEN
                } else {
                    RED
                }
            } else {
                BLUE
            };
            writeln!(
                buffer,
                "{: >15} {}{:=>used$}{}{:->remaining$} {}/{}\x1b[0K",
                self.name,
                bar_colour,
                "",
                CLEAR,
                "",
                current,
                total,
                used = used,
                remaining = remaining
            )
            .unwrap();
        } else {
            // a bar without a known total has a looping 10 wide bar
            let draws = self.draws;
            if !aborted {
                self.draws = draws + 1;
            }

            let start_point = draws % len;
            let line = if finished && !aborted {
                format!("{}{:=>len$}{}", GREEN, "", CLEAR, len = len)
            } else {
                let mut buffer = String::new();
                for i in 0..len {
                    if (i >= start_point && i < start_point + 10) || (i + len < start_point + 10) {
                        write!(buffer, "{}={}", if aborted { RED } else { BLUE }, CLEAR).unwrap()
                    } else {
                        buffer.push('-')
                    }
                }
                buffer
            };
            writeln!(buffer, "{: >15} {} {}\x1b[0K", self.name, line, current,).unwrap();
        }
        buffer
    }
}

struct BrailleState {
    current: AtomicUsize,
    total: AtomicUsize,
    done: AtomicBool,
    aborted: AtomicBool,
}

/// A handle to the boring bar that can used to update the progress of the bar.
#[derive(Clone)]
pub struct BrailleHandle {
    state: Arc<BrailleState>,
    multibar: BarState,
}

/// The handle used to update the progress bar.
impl BrailleHandle {
    /// Increment the progress by the given amount.
    pub fn tick(&self) {
        self.state.current.fetch_add(1, Ordering::Release);
        self.multibar.redraw()
    }

    /// Finish rendering the bar. It will be redrawn once more and never again.
    pub fn finish(&self) {
        self.state.done.store(true, Ordering::Release);
        self.multibar.redraw()
    }

    /// Finish rendering the bar. It will be redrawn once more and never again.
    pub fn abort(&self) {
        self.state.aborted.store(true, Ordering::Release);
        self.multibar.redraw()
    }
}

/// A builder for the boring bar. Right now you can only specify a name and a total. Use a total
/// of `0` for a looping progress bar.
pub struct BrailleBuilder {
    name: String,
    total: usize,
    text: Option<Box<dyn FnMut(usize,usize) -> String + Send + Sync + 'static>>,
}

impl BrailleBuilder {
    /// A progress bar with the total size. If the total is 0, progress will be shown as a rolling
    /// update.
    pub fn new(name: String, total: usize) -> BrailleBuilder {
        BrailleBuilder { name, total, text: None }
    }

    pub fn with_text<F: FnMut(usize,usize) -> String + Send + Sync + 'static>(&mut self, f: Box<F>) {
        self.text = Some(f)
    }
}

impl BarBuild for BrailleBuilder {
    type Handle = BrailleHandle;
    type Drawer = BrailleDrawer;

    fn build(self, multibar: BarState) -> (Self::Handle, Self::Drawer) {
        let state = Arc::new(BrailleState {
            current: AtomicUsize::new(0),
            total: AtomicUsize::new(self.total),
            done: AtomicBool::new(false),
            aborted: AtomicBool::new(false),
        });
        let handle = BrailleHandle {
            state: state.clone(),
            multibar,
        };
        let drawer = BrailleDrawer {
            name: self.name,
            draws: 0,
            last_ticker: 0,
            state,
            chars: BRAILE_SPINNER.chars().cycle().peekable(),
            text: self.text,
        };
        (handle, drawer)
    }
}

use std::str::Chars;
use std::iter::{Cycle, Peekable};

/// This is the `BrailleDrawer`.
pub struct BrailleDrawer {
    name: String,
    draws: usize,
    last_ticker: usize,
    state: Arc<BrailleState>,
    chars: Peekable<Cycle<Chars<'static>>>,
    text: Option<Box<dyn FnMut(usize,usize) -> String + Send + Sync>>,
}


const BRAILE_SPINNER: &str = "⠁⠁⠉⠙⠚⠒⠂⠂⠒⠲⠴⠤⠄⠄⠤⠠⠠⠤⠦⠖⠒⠐⠐⠒⠓⠋⠉⠈⠈";

// const BRAILE_PROGRESS_FLICKER: &str = "⡀⢀⣄⣠⣦⣴⣷⣾";
// const BRAILE_PROGRESS_FLICKER_CHARS: usize = 8;

// const BRAILE_PROGRESS_TICK: &str = " ⡀⢀⣀⣄⣠⣤⣦⣴⣶⣷⣾⣿";
// const BRAILE_PROGRESS_TICK_CHARS: usize = 13;

const BRAILE_PROGRESS: &str = " ⡀⣀⣄⣤⣦⣶⣷⣿";
const BRAILE_PROGRESS_CHARS: usize = 9;

impl BarDraw for BrailleDrawer {
    fn is_finished(&self) -> bool {
        self.state.done.load(Ordering::Acquire)
    }

    fn draw(&mut self, params: &BarDrawParams) -> String {
        let current = self.state.current.load(Ordering::Acquire);
        let total = self.state.total.load(Ordering::Acquire);
        let was_finished = self
            .state
            .done
            .fetch_or(params.finished(), Ordering::Acquire);
        let was_aborted = self
            .state
            .aborted
            .fetch_or(params.aborted(), Ordering::Acquire);
        let finished = was_finished || params.finished();
        let aborted = was_aborted || params.aborted();
        if total == 0 {
            let colour = if aborted { RED } else if finished {GREEN} else {BLUE};

            if self.last_ticker != current {
                self.last_ticker = current;
                self.draws += 1;
                if self.draws % 2 == 0 {
                    self.chars.next();
                }
            }

            let char = if finished && !aborted { &'⣿' } else { self.chars.peek().unwrap() };

            format!("{}{}{} {}\x1b[0K\n", colour, char, CLEAR, self.name)
        } else {
            if self.last_ticker != current {
                self.last_ticker = current;
                self.draws += 1;
            }

            let colour = if aborted { RED } else if finished {GREEN} else {BLUE};
            let char = if current == 0 {
                ' '
            } else {
                let ix = (current * (BRAILE_PROGRESS_CHARS - 1)) / total;
                BRAILE_PROGRESS.chars().nth(ix).unwrap()
            };
            let prog = if let Some(ref mut f) = &mut self.text {
                f(current, total)
            } else {
                format!("({:.0}%)", ((100 * current) as f64) / (total as f64))
            };
            format!("{}{}{} {} {}\x1b[0K\n", colour, char, CLEAR, self.name, prog)

            // let char = if current == 0 {
            //     ' '
            // } else {
            //     let n = (current * BRAILE_PROGRESS_CHARS) / total;
            //     let nf = n + ((self.draws / 4) % 2);
            //     BRAILE_PROGRESS.chars().nth(nf).unwrap_or('⣿')
            // };
            // format!("{}{}{} {}\n", colour, char, CLEAR, self.name)

            // let char = if current == 0 {
            //     ' '
            // } else {
            //     let tier = (current * 4) / total;
            //     let n = 2 * tier + ((self.draws / 4) % 2);
            //     BRAILE_PROGRESS_FLICKER.chars().nth(n).unwrap_or('⣿')
            // };
            // format!("{}{}{} {}\n", colour, char, CLEAR, self.name)

            // let (char1, char2) = if current == 0 {
            //     (' ', ' ')
            // } else {
            //     let tier = (current * 4) / total;
            //     let (d1, d2) = match (self.draws / 4) % 4 {
            //         0 => (1, 0),
            //         1 => (2, 0),
            //         2 => (0, 1),
            //         _ => (0, 2),
            //     };
            //     let n1 = 3 * tier + d1;
            //     let n2 = 3 * tier + d2;
            //     (BRAILE_PROGRESS_TICK.chars().nth(n1).unwrap_or('⣿'),
            //      BRAILE_PROGRESS_TICK.chars().nth(n2).unwrap_or('⣿')
            //      )
            // };
            // format!("{}{}{}{} {}\n", colour, char1, char2, CLEAR, self.name)
        }
    }
}
