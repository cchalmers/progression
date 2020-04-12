use core::sync::atomic::Ordering;
use core::sync::atomic::{AtomicBool, AtomicUsize};
use futures::channel::mpsc;
use futures::task::AtomicWaker;
use futures_timer::Delay;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

struct ProgressBarInner {
    name: String,
    total: AtomicUsize,
    current: AtomicUsize,
    // waker: Arc<AtomicWaker>,
    state: Arc<BarState>,
    // done: Arc<AtomicBool>,
}

#[derive(Clone)]
pub struct ProgressBar {
    inner: Arc<ProgressBarInner>,
}

impl ProgressBar {
    pub fn tick(&self) {
        self.incr(1);
    }

    pub fn set(&self, val: usize) {
        let inner = &self.inner;
        inner.current.store(val, Ordering::Release);
        inner.state.changed.store(true, Ordering::Release);
        inner.state.waker.wake();
    }

    pub fn incr(&self, incr: usize) {
        let inner = &self.inner;
        inner.current.fetch_add(incr, Ordering::AcqRel);
        inner.state.changed.store(true, Ordering::Release);
        inner.state.waker.wake();
    }

    pub fn total(&self) -> usize {
        self.inner.total.load(Ordering::Acquire)
    }
}

pub struct ProgressBarBuilder {
    name: String,
    total: usize,
    starting: usize,
}

impl ProgressBarBuilder {
    pub fn new(name: String, total: usize) -> ProgressBarBuilder {
        ProgressBarBuilder {
            name,
            total,
            starting: 0,
        }
    }
}

/// Shared state between the handle and runner.
pub struct BarState {
    /// A signal that the multibar can finish on the next call to poll.
    done: AtomicBool,
    /// A signal the progress bar state has changed and we should redraw.
    changed: AtomicBool,
    /// Reference to our own waker so we can register new wakers.
    waker: AtomicWaker,
}

#[derive(Clone)]
pub struct MultiBarHandle {
    bar_sender: mpsc::Sender<ProgressBar>,
    state: Arc<BarState>,
}

impl MultiBarHandle {
    pub fn add_bar(
        &mut self,
        builder: ProgressBarBuilder,
    ) -> Result<ProgressBar, mpsc::TrySendError<ProgressBar>> {
        let inner = ProgressBarInner {
            name: builder.name,
            current: AtomicUsize::new(builder.starting),
            total: AtomicUsize::new(builder.total),
            state: self.state.clone(),
        };
        let bar = ProgressBar {
            inner: Arc::new(inner),
        };
        self.bar_sender.try_send(bar.clone())?;
        Ok(bar)
    }

    pub fn finish(&mut self) {
        self.state.done.store(true, Ordering::Release);
        self.state.waker.wake();
    }
}

pub fn multi_bar() -> (MultiBarHandle, MultiBarFuture) {
    let (bar_sender, bar_receiver) = mpsc::channel(128);
    let shared_state = Arc::new(BarState {
        done: AtomicBool::new(false),
        changed: AtomicBool::new(false),
        waker: AtomicWaker::new(),
    });

    let handle = MultiBarHandle {
        bar_sender,
        state: shared_state.clone(),
    };
    let future = MultiBarFuture {
        state: shared_state,
        bar_receiver,
        waiting_delay: false,
        delay: Delay::new(Duration::from_millis(41)),
        bars: vec![],
        prev_num_bars: 0,
        receiver_finished: false,
    };
    (handle, future)
}

pub struct MultiBarFuture {
    state: Arc<BarState>,

    /// We're waiting on the delay before attemptint to do any redrawing.
    waiting_delay: bool,

    /// Receiver of new progress bars to add.
    bar_receiver: mpsc::Receiver<ProgressBar>,

    /// A delay to prevent redrawing too fast.
    delay: Delay,

    /// Current stack of progress bars to draw.
    bars: Vec<ProgressBar>,

    /// The number of bars we drew last time (so we know how many lines to clear before redrawing).
    prev_num_bars: usize,

    receiver_finished: bool,
}

impl Future for MultiBarFuture {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<()> {
        let runner = self.get_mut();

        if runner.waiting_delay {
            // First check that we're not trying to redraw too soon, if we are wait until it's
            // finished.
            if let Poll::Pending = Delay::poll(Pin::new(&mut runner.delay), cx) {
                return Poll::Pending;
            }
        }
        runner.waiting_delay = false;

        // must register the waker before checking state to prevent race conditions
        runner.state.waker.register(&cx.waker());

        let done = runner.state.done.load(Ordering::Acquire);

        // Check if the progress bar state has changed. If it has, store a `false` atomically.
        if runner
            .state
            .changed
            .compare_and_swap(true, false, Ordering::AcqRel)
        {
            // Collect any new bars added. If we see a none it means the sender has finished and we
            // shouldn't call `try_next` again.
            if !runner.receiver_finished {
                while let Ok(res) = runner.bar_receiver.try_next() {
                    match res {
                        Some(pb) => runner.bars.push(pb),
                        None => {
                            runner.receiver_finished = true;
                            break;
                        }
                    }
                }
            }

            draw_bars(&runner.bars, runner.prev_num_bars);
            runner.prev_num_bars = runner.bars.len();
            runner.waiting_delay = true;
        }

        if done {
            return Poll::Ready(());
        }

        if runner.waiting_delay {
            runner.delay.reset(Duration::from_millis(41));
        }

        Poll::Pending
    }
}

pub const RED: &str = "\u{1b}[49;31m";
pub const GREEN: &str = "\u{1b}[49;32m";
pub const YELLOW: &str = "\u{1b}[49;33m";
pub const CLEAR: &str = "\u{1b}[0m";

fn draw_bars(bars: &[ProgressBar], prev_num_bars: usize) {
    if bars.is_empty() {
        return;
    }
    use std::fmt::Write;
    let mut buffer = String::new();
    let max_height = term_size::dimensions().map(|x| x.1 - 1).unwrap_or(1);
    if prev_num_bars != 0 {
        write!(buffer, "\x1b[{}A", std::cmp::min(max_height, prev_num_bars)).unwrap();
    }
    let lower = if bars.len() > max_height {
        bars.len() - max_height
    } else {
        0
    };
    for bar in &bars[lower..] {
        let total = bar.inner.total.load(Ordering::Acquire);
        let current = bar.inner.current.load(Ordering::Acquire);
        let len = 80;
        let used = std::cmp::min(len, (len as f64 * (current as f64 / total as f64)).round() as usize);
        let remaining = len - used;
        writeln!(
            buffer,
            "{: >15} {}{:=>used$}{}{:->remaining$} {}/{}\x1b[0K",
            bar.inner.name,
            GREEN,
            "",
            CLEAR,
            "",
            current,
            total,
            used = used,
            remaining = remaining
        )
        .unwrap();
    }
    eprint!("{}", buffer);
}
