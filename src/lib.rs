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
    finished: AtomicBool,
    current: AtomicUsize,
    // waker: Arc<AtomicWaker>,
    state: Arc<MultiBarState>,
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

    pub fn finish(&self) {
        let inner = &self.inner;
        inner.finished.store(true, Ordering::Release);
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

struct WhenDoneInner {
    done: AtomicBool,
    waker: AtomicWaker,
}

/// Future that returns when the progress bar has handled the state when this was created
/// (redrawing is necessary).
pub struct WhenDone {
    inner: Arc<WhenDoneInner>,
}

impl WhenDone {
    fn new() -> (WhenDone, WhenDone) {
        let inner = Arc::new(WhenDoneInner {
            done: AtomicBool::new(false),
            waker: AtomicWaker::new(),
        });
        (
            WhenDone {
                inner: inner.clone(),
            },
            WhenDone { inner },
        )
    }

    fn done(&self) {
        self.inner.done.store(true, Ordering::Release);
        self.inner.waker.wake();
    }
}

impl Future for WhenDone {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let inner = &self.get_mut().inner;
        inner.waker.register(&cx.waker());
        if inner.done.load(Ordering::Acquire) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

/// Shared state between the handle and runner.
pub struct MultiBarState {
    /// A signal that the multibar can finish on the next call to poll.
    done: AtomicBool,
    /// A signal the progress bar state has changed and we should redraw.
    changed: AtomicBool,
    /// All currently active bars are considered finished. They will be drawn once more and never
    /// again.
    finish_active: AtomicBool,
    /// Stop drawing the progress bar, can be resumed later.
    stop: AtomicBool,
    /// Reference to our own waker so we can register new wakers.
    waker: AtomicWaker,
}

#[derive(Clone)]
pub struct MultiBarHandle {
    bar_sender: mpsc::Sender<ProgressBar>,
    when_done_sender: mpsc::UnboundedSender<WhenDone>,
    state: Arc<MultiBarState>,
}

impl MultiBarHandle {
    pub fn add_bar(
        &mut self,
        builder: ProgressBarBuilder,
    ) -> Result<ProgressBar, mpsc::TrySendError<ProgressBar>> {
        let inner = ProgressBarInner {
            name: builder.name,
            finished: AtomicBool::new(false),
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

    /// Stop the multibar from rendering any more. Rendering may be resumed by setting `resume`.
    pub fn stop(&self) {
        self.state.stop.store(true, Ordering::Release);
        self.state.waker.wake();
    }

    /// Resume multibar rendering.
    pub fn resume(&self) {
        self.state.stop.store(false, Ordering::Release);
        self.state.waker.wake();
    }

    /// Inform the MultiBar that it is done. It will render once more and then the future will
    /// return a ready.
    pub fn finish(&self) {
        self.state.done.store(true, Ordering::Release);
        self.state.waker.wake();
    }

    /// Check if the has been marked as finished (via `finish`).
    pub fn is_finished(&self) -> bool {
        self.state.done.load(Ordering::Acquire)
    }

    /// All currently loaded bars will be marked as finished, if they're not complete the progress
    /// bar will turn red. This is useful for when the work the multibar is tracking is
    /// interrupted.
    pub fn finish_active(&self) {
        self.state.finish_active.store(true, Ordering::Release);
        self.state.waker.wake();
    }

    /// When until a render at the current state (or a later state) has been performed. Returns
    /// `None` if the `MultiBar` has been dropped.
    pub fn wait(&mut self) -> Option<WhenDone> {
        let (future, when_done) = WhenDone::new();
        match self.when_done_sender.unbounded_send(when_done) {
            Ok(()) => {
                self.state.waker.wake();
                Some(future)
            }
            Err(send_err) => {
                // this should be the only case when an unbounded sender
                assert!(send_err.is_disconnected());
                None
            }
        }
    }
}

pub fn multi_bar() -> (MultiBarHandle, MultiBarFuture) {
    let (bar_sender, bar_receiver) = mpsc::channel(128);
    let (when_done_sender, when_done_receiver) = mpsc::unbounded();
    let shared_state = Arc::new(MultiBarState {
        done: AtomicBool::new(false),
        changed: AtomicBool::new(false),
        finish_active: AtomicBool::new(false),
        stop: AtomicBool::new(false),
        waker: AtomicWaker::new(),
    });

    let handle = MultiBarHandle {
        when_done_sender,
        bar_sender,
        state: shared_state.clone(),
    };
    let future = MultiBarFuture {
        state: shared_state,
        bar_receiver,
        when_done_receiver,
        waiting_delay: false,
        delay: Delay::new(Duration::from_millis(41)),
        finished_bars: vec![],
        active_bars: vec![],
        prev_num_bars: 0,
        bar_receiver_finished: false,
        done_receiver_finished: false,
        stopped: false,
    };
    (handle, future)
}

pub struct MultiBarFuture {
    state: Arc<MultiBarState>,

    /// We're waiting on the delay before attemptint to do any redrawing.
    waiting_delay: bool,

    /// Receiver of new progress bars to add.
    bar_receiver: mpsc::Receiver<ProgressBar>,

    /// Notify interested parties when a render has occured.
    when_done_receiver: mpsc::UnboundedReceiver<WhenDone>,

    /// A delay to prevent redrawing too fast.
    delay: Delay,

    /// Current stack of progress bars to draw.
    finished_bars: Vec<ProgressBar>,

    /// Current stack of progress bars to draw.
    active_bars: Vec<ProgressBar>,

    /// The number of bars we drew last time (so we know how many lines to clear before redrawing).
    prev_num_bars: usize,

    bar_receiver_finished: bool,
    done_receiver_finished: bool,
    stopped: bool,
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
        runner.stopped = runner.state.stop.load(Ordering::Acquire);

        // See if anyone's waiting to hear back from us, tell them we're up to date. We queue them
        // up here and notify after in case wait was called while rendering the bar.
        let mut wd_queue = vec![];
        if !runner.done_receiver_finished {
            while let Ok(res) = runner.when_done_receiver.try_next() {
                match res {
                    Some(wd) => wd_queue.push(wd),
                    None => {
                        runner.done_receiver_finished = true;
                        break;
                    }
                }
            }
        }

        // Check if the progress bar state has changed. If it has, store a `false` atomically.
        if !runner.stopped
            && runner
                .state
                .changed
                .compare_and_swap(true, false, Ordering::AcqRel)
        {
            // Collect any new bars added. If we see a none it means the sender has finished and we
            // shouldn't call `try_next` again.
            if !runner.bar_receiver_finished && !runner.state.done.load(Ordering::Acquire) {
                while let Ok(res) = runner.bar_receiver.try_next() {
                    match res {
                        Some(pb) => runner.active_bars.push(pb),
                        None => {
                            runner.bar_receiver_finished = true;
                            break;
                        }
                    }
                }
            }

            let active = &mut runner.active_bars;
            let finished = &mut runner.finished_bars;

            let mut removed = 0;

            if runner
                .state
                .finish_active
                .compare_and_swap(true, false, Ordering::AcqRel)
            {
                removed = active.len();
                // eww
                active
                    .iter_mut()
                    .for_each(|bar| bar.inner.finished.store(true, Ordering::Relaxed));
                finished.append(active);
            } else {
                for i in 0..active.len() {
                    let i = i - removed;
                    if active[i].inner.finished.load(Ordering::Acquire) {
                        let bar = active.remove(i);
                        finished.push(bar);
                        removed += 1;
                    }
                }
            }

            draw_bars(
                active,
                &finished[finished.len() - removed..],
                runner.prev_num_bars,
            );
            runner.prev_num_bars = runner.active_bars.len();
            runner.waiting_delay = true;
        }

        if runner.stopped {
            runner.prev_num_bars = 0;
        }

        wd_queue.iter().for_each(WhenDone::done);

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
pub const BLUE: &str = "\u{1b}[49;34m";
pub const CLEAR: &str = "\u{1b}[0m";

fn draw_bars(
    active_bars: &[ProgressBar],
    newly_finished_bars: &[ProgressBar],
    prev_num_bars: usize,
) {
    if active_bars.is_empty() && newly_finished_bars.is_empty() {
        return;
    }
    use std::fmt::Write;
    let mut buffer = String::new();
    let max_height = term_size::dimensions().map(|x| x.1 - 1).unwrap_or(1);
    if prev_num_bars != 0 {
        write!(buffer, "\x1b[{}A", std::cmp::min(max_height, prev_num_bars)).unwrap();
    }
    // TODO active can still more then max_height
    // let lower = if active_bars.len() > max_height {
    //     active_bars.len() - max_height
    // } else {
    //     0
    // };
    for bar in newly_finished_bars.iter().chain(active_bars) {
        // for bar in &active_bars[lower..] {
        let total = bar.inner.total.load(Ordering::Acquire);
        let current = bar.inner.current.load(Ordering::Acquire);
        let finished = bar.inner.finished.load(Ordering::Acquire);
        let len = 80;
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
            bar.inner.name,
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
    }
    eprint!("{}", buffer);
}
