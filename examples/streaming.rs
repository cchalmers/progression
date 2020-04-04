use futures::executor::LocalPool;
use futures::stream::StreamExt;
use futures::task::LocalSpawnExt;
use std::future::Future;
use std::pin::Pin;
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

fn main() {
    eprintln!("starting");
    let sizes = vec![
        800_000, 1_384_291, 400_000, 213_321, 2_221_002, 392_292, 994_231,
    ];

    let (mut handle, future) = progression::multi_bar();

    let bars: Vec<_> = sizes
        .into_iter()
        .cycle()
        .take(300)
        .enumerate()
        .map(|(i, size)| (i, size, handle.clone()))
        .collect();

    let mut pool = LocalPool::new();
    let spawner = pool.spawner();

    spawner.spawn_local(future).unwrap();

    let stream = futures::stream::iter(bars)
        .map(|(i, size, mut handle)| async move {
            let builder = progression::ProgressBarBuilder::new(format!("dl_{}", i), size);
            let bar = handle.add_bar(builder).unwrap();
            for _ in 0_usize..bar.total() {
                // Delay::new(Duration::from_millis(1));
                yield_now().await;
                bar.tick();
            }
        })
        .buffer_unordered(4);

    spawner
        .spawn_local(async move {
            stream.collect::<Vec<_>>().await;
            handle.finish()
        })
        .unwrap();

    pool.run();

    println!();
    println!("Hello, world!");
}

