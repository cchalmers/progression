use bitfield::*;
use futures::task::AtomicWaker;
use std::fmt::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

bitfield! {
    struct BarStateField(u8);
    impl Debug;
    /// Notify the handler that this bar has change and should be redrawn.
    changed, set_changed: 0;

    /// Mark this bar as finished. It will drawn once (before drawing any active bars) and left
    /// there.
    finished, set_finished: 1;

    /// Clear this bar from the stack of bars. Draw will not be called while this is set. Changed
    /// should be set when changing this parameter.
    invisible, set_invisible: 2;
}

/// Shared state between the handle and runner.
#[derive(Debug)]
struct MultiBarStateInner {
    field: AtomicUsize,
    waker: AtomicWaker,
}

#[derive(Debug, Clone)]
pub struct MultiBarState {
    inner: Arc<MultiBarStateInner>,
}

impl MultiBarState {
    fn update(&self, field: BarStateField) {
        self.inner
            .field
            .fetch_or(field.0 as usize, Ordering::Release);
        self.inner.waker.wake()
    }

    /// Finish the bar. I will be redrawn once more (assuming the multi bar gets polled again) and
    /// never again.
    pub fn finish(&self) {
        let mut field = BarStateField(0);
        field.set_changed(true);
        field.set_finished(true);
        self.update(field)
    }

    /// Mark the bar as being needed to be redrawn.
    pub fn redraw(&self) {
        let mut field = BarStateField(0);
        field.set_changed(true);
        self.update(field)
    }
}

/// A bar that knows how to draw its self. The bar is stored in the MultiBarFuture, which calls
/// `draw` when nessesary.
pub trait DrawBar {
    type Handle;
    type Builder;

    /// Build the bar given a handle for that bar. The `MultiBarState` will typically be stored in
    /// the `Handle`.
    fn build(builder: Self::Builder, multibar: MultiBarState) -> (Self::Handle, Self);

    /// Draw the bar in its current state. The bar should only take a single line.
    fn draw(&mut self) -> String;

    /// This multibar handle has canceled all current progress bars. This can be used to change the
    /// state of the bar so that the next time it's drawn it might look different (has default
    /// implementation to do nothing).
    fn cancel(&mut self) {}
}

struct BoringBarState {
    total: AtomicUsize,
    current: AtomicUsize,
    finished: AtomicBool,
}

/// A handle to the boring bar that can
pub struct BoringBarHandle {
    state: Arc<BoringBarState>,
    multibar: MultiBarState,
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

/// This is the `BoringBarDrawer`.
struct BoringBarDrawer {
    name: String,
    draws: usize,
    state: Arc<BoringBarState>,
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

impl DrawBar for BoringBarDrawer {
    type Handle = BoringBarHandle;
    type Builder = BoringBarBuilder;

    fn build(builder: Self::Builder, multibar: MultiBarState) -> (Self::Handle, Self) {
        let state = Arc::new(BoringBarState {
            total: AtomicUsize::new(builder.total),
            current: AtomicUsize::new(0),
            finished: AtomicBool::new(false),
        });
        let handle = BoringBarHandle {
            state: state.clone(),
            multibar,
        };
        let drawer = BoringBarDrawer {
            name: builder.name,
            draws: 0,
            state,
        };
        (handle, drawer)
    }

    fn draw(&mut self) -> String {
        let mut buffer = String::new();
        let total = self.state.total.load(Ordering::Acquire);
        let current = self.state.current.load(Ordering::Acquire);
        let finished = self.state.finished.load(Ordering::Acquire);
        let len = 80;
        if total != 0 {
            let used = std::cmp::min(
                len,
                (len as f64 * (current as f64 / total as f64)).round() as usize,
            );
            let remaining = len - used;
            let bar_colour = if finished {
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
            self.draws = draws + 1;

            let start_point = draws % len;
            let line = if finished {
                format!("{}{:=>len$}{}", GREEN, "", CLEAR, len = len)
            } else {
                let mut buffer = String::new();
                for i in 0..len {
                    if (i >= start_point && i < start_point + 10) || (i + len < start_point + 10) {
                        write!(buffer, "{}={}", BLUE, CLEAR).unwrap()
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
