use std::collections::VecDeque;
use std::path::PathBuf;

use tokio::fs;
use tokio::io;

use progression::{multi_bar, MultiBarHandle, ProgressBar, ProgressBarBuilder};

#[tokio::main]
async fn main() -> io::Result<()> {
    let mut args = std::env::args();
    let prog = args.next().unwrap();
    if args.len() != 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Usage: {} <from> <to>", prog),
        ));
    }

    let from = args.next().unwrap();
    let to = args.next().unwrap();

    let (handle, future) = multi_bar();
    let future_join = tokio::spawn(future);

    let mut job = CopyJob::new(&mut handle.clone(), PathBuf::from(from), PathBuf::from(to)).await?;
    job.run().await?;

    handle.finish();
    future_join.await?;
    Ok(())
}

struct CopyJob {
    bar: ProgressBar,
    dirs: VecDeque<(PathBuf, usize)>,
    files: Vec<(PathBuf, PathBuf, usize)>,
}

impl CopyJob {
    async fn new(handle: &mut MultiBarHandle, from: PathBuf, to: PathBuf) -> io::Result<CopyJob> {
        if !from.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "This cp only supports <from> as a directory",
            ));
        }

        let mut dirs = VecDeque::new();
        let mut files = Vec::new();
        let mut total_size = 0;
        eprintln!("{} -> {}", from.to_str().unwrap(), to.to_str().unwrap());

        let mut to_visit = vec![from.clone()];

        let scoping_bar = handle
            .add_bar(ProgressBarBuilder::new("scoping".to_string(), 0))
            .unwrap();
        while let Some(f) = to_visit.pop() {
            scoping_bar.tick();
            let t = to.join(f.strip_prefix(&from).unwrap());
            let meta = fs::metadata(&f).await?;
            total_size += meta.len() as usize;

            if meta.is_dir() {
                dirs.push_back((t, meta.len() as usize));

                let mut children = fs::read_dir(&f).await?;
                while let Some(child) = children.next_entry().await? {
                    to_visit.push(child.path());
                }
            } else {
                files.push((f, t, meta.len() as usize))
            }
        }
        scoping_bar.finish();

        let bar_builder = ProgressBarBuilder::new("copying".to_string(), total_size);
        if let Ok(bar) = handle.add_bar(bar_builder) {
            Ok(Self { bar, dirs, files })
        } else {
            Err(io::Error::new(
                io::ErrorKind::Other,
                "Unable to create a progress bar",
            ))
        }
    }

    async fn run(&mut self) -> io::Result<()> {
        for (dir, size) in self.dirs.iter() {
            fs::create_dir(dir).await?;
            self.bar.incr(*size);
        }

        for (from, to, size) in self.files.iter() {
            fs::copy(from, to).await?;
            self.bar.incr(*size);
        }

        self.bar.finish();

        Ok(())
    }
}
