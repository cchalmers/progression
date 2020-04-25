use futures::executor::LocalPool;
use futures::stream::Stream;
use futures::stream::StreamExt;
use futures::task::AtomicWaker;
use futures::task::LocalSpawnExt;
use nix::sys::signal;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

/// Yield the current task to the back of the queue, giving others a chance.
async fn yield_now() {
    /// Yield implementation
    struct YieldNow {
        yielded: bool,
    }

    impl Future for YieldNow {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.yielded {
                return Poll::Ready(());
            }

            self.yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }

    YieldNow { yielded: false }.await
}

// A ^Z handler. The StopStream will generate a stream a signals for every SIGTSTP.
static TSTP: AtomicBool = AtomicBool::new(false);
static STOP_TSTP: AtomicBool = AtomicBool::new(false);
lazy_static::lazy_static! {
    static ref TSTP_WAKER: AtomicWaker = AtomicWaker::new();
}
struct StopStream;
impl StopStream {
    fn stop() {
        STOP_TSTP.store(true, Ordering::Release);
        TSTP_WAKER.wake();
    }
}
// Only ever have one active stream from this at once!
impl Stream for StopStream {
    type Item = ();
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<()>> {
        TSTP_WAKER.register(&cx.waker());
        if STOP_TSTP.load(Ordering::Acquire) {
            Poll::Ready(None)
        } else if TSTP.load(Ordering::Acquire) {
            TSTP.store(false, Ordering::Release);
            Poll::Ready(Some(()))
        } else {
            Poll::Pending
        }
    }
}
pub fn suspend() {
    use nix::unistd::Pid;
    // suspend the whole process group
    signal::kill(Pid::from_raw(0), signal::SIGTSTP).unwrap();
}

fn main() {
    eprintln!("starting");
    let sizes = vec![
        800_000, 1_384_291, 400_000, 213_321, 2_221_002, 392_292, 994_231,
    ];

    let (handle, future) = progression::multi_bar();

    let ctrlc_handle = handle.clone();
    ctrlc::set_handler(move || {
        let ctrlc_handle = ctrlc_handle.clone();
        ctrlc_handle.finish_active();
        ctrlc_handle.finish();
    })
    .unwrap();

    // ^Z handler
    // This will gracefully resume when `fg`ing. If you bg it will also resume but this is probably
    // not what you want. It is possible to handle the bg case (by attempting to change the
    // termios, which will send another interrupt if we're not in the foreground) but this starts
    // to get gnarly so I'll leave it for now.
    extern "C" fn sigstop_handler(_: i32) {
        TSTP.store(true, Ordering::Release);
        TSTP_WAKER.wake();
    }
    let sigstop = signal::SigAction::new(
        signal::SigHandler::Handler(sigstop_handler),
        signal::SaFlags::empty(),
        signal::SigSet::empty(),
    );
    let mut old_action = unsafe { signal::sigaction(signal::Signal::SIGTSTP, &sigstop).unwrap() };

    let bars: Vec<_> = sizes
        .into_iter()
        .cycle()
        .take(10)
        .enumerate()
        .map(|(i, size)| (i, size, handle.clone()))
        .collect();

    let mut pool = LocalPool::new();
    let spawner = pool.spawner();

    spawner.spawn_local(future).unwrap();
    let stp_handle = handle.clone();
    spawner
        .spawn_local(StopStream.for_each(move |_| {
            let mut stp_handle = stp_handle.clone();
            async move {
                stp_handle.stop();
                stp_handle.wait().unwrap().await;
                let prev_action =
                    unsafe { signal::sigaction(signal::Signal::SIGTSTP, &old_action).unwrap() };
                suspend();
                old_action =
                    unsafe { signal::sigaction(signal::Signal::SIGTSTP, &prev_action).unwrap() };
                stp_handle.resume();
            }
        }))
        .unwrap();

    let stream = futures::stream::iter(bars)
        .map(|(i, size, mut handle)| async move {
            let sz = if size > 1_200_000 { 0 } else { size };
            let builder = progression::ProgressBarBuilder::new(format!("dl_{}", i), sz);
            if let Ok(bar) = handle.add_bar(builder) {
                for i in 0..std::cmp::min(1_200_000, size) {
                    if i % 4096 == 0 {
                        if handle.is_finished() {
                            return;
                        }
                    }
                    // Delay::new(Duration::from_millis(1));
                    yield_now().await;
                    bar.tick();
                }
                bar.finish();
            }
        })
        .buffer_unordered(4);

    spawner
        .spawn_local(async move {
            stream.collect::<Vec<_>>().await;
            handle.finish();
            StopStream::stop()
        })
        .unwrap();

    let _cap = NoEcho::new();
    pool.run();

    println!();
    println!("Hello, world!");
}

/// This a questionable struct that prevents echoings and returns to stop people typing while our
/// beautiful progress bars are being rendered, messing everything up. It's not really advisable to
/// to this, one of the main reasons being that the shell inherits this behavour if the destructor
/// isn't run. (One possible solution to this is to make a dtor function (using the ctor crate) or
/// use std::rt::at_exit (unstable))
pub struct NoEcho {
    original: termios::Termios,
}

impl NoEcho {
    pub fn new() -> NoEcho {
        use termios::*;
        let fd = 0;
        let mut termios = Termios::from_fd(fd).unwrap();
        let original = termios.clone();
        termios.c_iflag |= termios::IGNCR;
        termios.c_lflag &= !termios::ECHO;
        termios::tcsetattr(fd, termios::TCSAFLUSH, &termios).unwrap();
        NoEcho { original }
    }
}

impl Drop for NoEcho {
    fn drop(&mut self) {
        termios::tcsetattr(0, termios::TCSAFLUSH, &self.original).unwrap();
    }
}
