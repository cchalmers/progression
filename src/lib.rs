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

pub mod draw;

bitfield::bitfield! {
    struct BarStateField(u8);
    impl Debug;
    // /// Notify the handler that this bar has change and should be redrawn.
    // changed, set_changed: 0;

    /// Mark this bar as finished. It will drawn once (before drawing any active bars) and left
    /// there.
    finished, set_finished: 1;

    /// Clear this bar from the stack of bars. Draw will not be called while this is set. Changed
    /// should be set when changing this parameter.
    invisible, set_invisible: 2;
}

/// Shared state between the handle and runner.
#[derive(Debug)]
struct BarStateInner {
    field: AtomicUsize,
    mbs: Arc<MultiBarState>,
}

#[derive(Debug, Clone)]
pub struct BarState {
    inner: Arc<BarStateInner>,
}

impl BarState {
    fn new(mbs: Arc<MultiBarState>) -> BarState {
        BarState {
            inner: Arc::new(BarStateInner {
                field: AtomicUsize::new(0),
                mbs,
                // waker
            }),
        }
    }
    fn update(&self, field: BarStateField) {
        self.inner
            .field
            .fetch_or(field.0 as usize, Ordering::Release);
        self.inner.mbs.changed.store(true, Ordering::Release);
        self.inner.mbs.waker.wake()
    }

    /// Finish the bar. It will be redrawn once more (assuming the multi bar gets polled again) and
    /// never again.
    pub fn finish(&self) {
        let mut field = BarStateField(0);
        // field.set_changed(true);
        field.set_finished(true);
        self.update(field)
    }

    /// Mark the bar as being needed to be redrawn.
    pub fn redraw(&self) {
        self.inner.mbs.changed.store(true, Ordering::Release);
        self.inner.mbs.waker.wake()
    }
}

/// A bar that knows how to draw itself. The bar is stored in the MultiBarFuture, which calls
/// `draw` when nessesary.
pub trait BarBuild {
    type Handle;
    type Drawer: BarDraw;

    /// Build the bar given a handle for that bar. The `BarState` will typically be stored in
    /// the `Handle`.
    fn build(self, multibar: BarState) -> (Self::Handle, Self::Drawer);
}

/// Parameters from the hander.
pub struct BarDrawParams {
    aborted: bool,
    finished: bool,
}

impl BarDrawParams {
    /// Has this bar been aborted by the handler?
    pub fn aborted(&self) -> bool {
        self.aborted
    }

    /// Has this bar been finished by the handler?
    pub fn finished(&self) -> bool {
        self.finished
    }
}

/// A bar that knows how to draw itself. The bar is stored in the MultiBarFuture, which calls
/// `draw` when nessesary.
pub trait BarDraw: Send {
    fn is_finished(&self) -> bool;

    /// Draw the bar in its current state. The bar should only take a single line.
    fn draw(&mut self, params: &BarDrawParams) -> String;

    /// This multibar handle has canceled all current progress bars. This can be used to change the
    /// state of the bar so that the next time it's drawn it might look different (has default
    /// implementation to do nothing).
    fn cancel(&mut self) {}
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
#[derive(Debug)]
pub struct MultiBarState {
    /// A signal that the multibar can finish on the next call to poll.
    done: AtomicBool,
    /// A signal the progress bar state has changed and we should redraw.
    changed: AtomicBool,
    /// All currently active bars are considered finished. They will be drawn once more and never
    /// again.
    abort_active: AtomicBool,
    /// Stop drawing the progress bar, can be resumed later.
    stop: AtomicBool,
    /// Reference to our own waker so we can register new wakers.
    waker: AtomicWaker,
}

#[derive(Clone)]
pub struct MultiBarHandle {
    bar_sender: mpsc::Sender<Box<dyn BarDraw>>,
    when_done_sender: mpsc::UnboundedSender<WhenDone>,
    state: Arc<MultiBarState>,
}

impl MultiBarHandle {
    pub fn add_bar<B: BarBuild + 'static>(
        &mut self,
        builder: B,
    ) -> Result<B::Handle, mpsc::TrySendError<Box<dyn BarDraw>>> {
        let bar_state = BarState::new(self.state.clone());
        let (handle, drawer) = builder.build(bar_state);
        self.bar_sender.try_send(Box::new(drawer))?;
        Ok(handle)
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

    /// All currently loaded bars will be marked as aborted. Bar may be rendered differently when
    /// aborted. This is useful for when the work the multibar is tracking is interrupted.
    pub fn abort_active(&self) {
        self.state.abort_active.store(true, Ordering::Release);
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
        abort_active: AtomicBool::new(false),
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

type BarDrawer = Box<dyn BarDraw>;

pub struct MultiBarFuture {
    state: Arc<MultiBarState>,

    /// We're waiting on the delay before attemptint to do any redrawing.
    waiting_delay: bool,

    /// Receiver of new progress bars to add.
    bar_receiver: mpsc::Receiver<BarDrawer>,

    /// Notify interested parties when a render has occured.
    when_done_receiver: mpsc::UnboundedReceiver<WhenDone>,

    /// A delay to prevent redrawing too fast.
    delay: Delay,

    /// Current stack of progress bars to draw.
    finished_bars: Vec<BarDrawer>,

    /// Current stack of progress bars to draw.
    active_bars: Vec<BarDrawer>,

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

            let mut params = BarDrawParams {
                finished: done,
                aborted: false,
            };

            if runner
                .state
                .abort_active
                .compare_and_swap(true, false, Ordering::AcqRel)
            {
                params.aborted = true;
                finished.append(active);
            } else {
                let mut removed = 0;
                for i in 0..active.len() {
                    let i = i - removed;
                    if active[i].is_finished() {
                        let bar = active.remove(i);
                        finished.push(bar);
                        removed += 1;
                    }
                }
            }

            draw_bars(active, finished, runner.prev_num_bars, params);
            finished.clear();
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

fn draw_bars(
    active_bars: &mut [Box<dyn BarDraw>],
    newly_finished_bars: &mut [Box<dyn BarDraw>],
    prev_num_bars: usize,
    params: BarDrawParams,
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
    for bar in newly_finished_bars.iter_mut().chain(active_bars) {
        write!(buffer, "{}", bar.draw(&params)).unwrap();
    }
    eprint!("{}", buffer);
}
