use tokio::spawn;
use tokio::io::Result;
use tokio::fs::{read_dir, DirEntry};
use futures::stream::TryStreamExt;

use progression::{ProgressBarBuilder, ProgressBar};

#[tokio::main]
async fn main() -> Result<()> {

    let (handle, future) = progression::multi_bar();
    let future_join = spawn(future);

    read_dir(".")
        .await?
        .try_for_each_concurrent(None, |entry| async {
            let mut handle = handle.clone();
            let name = entry.file_name().into_string().unwrap();
            let builder = ProgressBarBuilder::new(format!("{:?}", name), 1);
            if let Ok(bar) = handle.add_bar(builder) {
                get_size(name, entry, bar).await.unwrap();
            }

            Ok(())
        }).await?;

    handle.finish();
    future_join.await?;

    Ok(())
}

async fn get_size(name: String, start: DirEntry, bar: ProgressBar) -> Result<()> {
    let mut to_visit = vec![start];
    let mut dirs_visited = 0;
    let mut dirs_to_visit = 1;
    let mut bytes = 0;

    while let Some(e) = to_visit.pop() {
        let meta = e.metadata().await?;
        bytes += meta.len();

        if meta.is_dir() {
            let mut children = read_dir(e.path()).await?;

            while let Some(child) = children.next_entry().await? {
                dirs_to_visit += 1;
                to_visit.push(child);
            }
        }

        dirs_visited += 1;

        bar.set(dirs_visited);
        bar.update_total(dirs_to_visit);
        bar.update_name(format!("{}: {}", name, bytes));
    }

    bar.finish();

    Ok(())
}
