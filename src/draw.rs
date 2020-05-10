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

/// A builder for the boring bar. Right now you can only speicify a name and a total. Use a total
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
