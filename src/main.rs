use clap::Parser;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Semaphore;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

mod ssh;
use ssh::SshTransfer;
mod utils;

const PARALLELISM: usize = 8;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Source directory or files
    #[clap(required = true)]
    source: PathBuf,

    /// Destination in format user@host:path or local/path
    #[clap(required = true)]
    destination: String,

    /// Number of parallel workers
    #[arg(short, long, default_value_t = PARALLELISM)]
    jobs: usize,

}

async fn send_file(
    src_root: PathBuf,
    dest_root: PathBuf,
    path: PathBuf,
    pb: ProgressBar
) -> anyhow::Result<()> {
    let src_path = src_root.join(&path);
    let dest_path = dest_root.join(&path);
    fs::create_dir_all(dest_path.parent().unwrap())?;
    let mut input = BufReader::new(File::open(&src_path)?);
    let mut output = BufWriter::new(File::create(&dest_path)?);
    let mut buffer = vec![0; 8192];
    let mut written = 0u64;
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        let data = &buffer[..n];
        output.write_all(data)?;
        written += n as u64;
        pb.set_position(written);
    }
    pb.finish_and_clear();
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let dest_parts = args.destination.split(":").collect::<Vec<_>>();

    if dest_parts.len() == 2 {
        cp_ssh_files(args).await?;
    } else if dest_parts.len() == 1 {
        cp_local_files(args).await?;
    } else {
        anyhow::bail!("Invalid destination format");
    }
    
    Ok(())
}

async fn cp_local_files(args: Args) -> anyhow::Result<()> {
    let src_root = Path::new(&args.source).parent().unwrap_or(&args.source);
    let dest_root = Path::new(&args.destination);
    println!("Copying from {} to {}", src_root.display(), dest_root.display());
    let m = Arc::new(MultiProgress::new());

    let semaphore = Arc::new(Semaphore::new(args.jobs));
    let mut handles = vec![];

    let walker = walkdir::WalkDir::new(&args.source);
    walker.into_iter().filter_map(Result::ok).for_each(|entry| {
        let path = entry.path();
        if path.is_file() {
            let size = path.metadata().unwrap().len();
            let src_root = src_root.to_path_buf();
            let dest_root = dest_root.to_path_buf();
            let path = path.strip_prefix(&src_root).unwrap().to_path_buf();
            println!("processing file2 :{}, {}", src_root.display(), path.display());
            let sem = semaphore.clone();
            let m = m.clone();

            let h = tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                
                let pb = m.add(ProgressBar::new(size));
                let sty = ProgressStyle::with_template("{msg} {bar:40} {bytes}/{total_bytes} ({eta})")
                    .unwrap()
                    .progress_chars("=>-");
                pb.set_style(sty);
                pb.set_message(utils::align_str(path.to_str().unwrap(), 20));
                let _ = send_file(src_root, dest_root, path, pb).await;
            });
            handles.push(h);
        }
    });

    // Wait for all transfers
    for h in handles {
        let _ = h.await;
    }

    println!("✅ Transfer completed!");
    Ok(())
}

async fn cp_ssh_files(args: Args) -> anyhow::Result<()> {
    // Parse destination
    let (ssh_dest, remote_path) = parse_ssh_destination(&args.destination)?;
    let remote_root = Path::new(&remote_path);

    // Connect via SSH
    println!("🔗 Connecting via SSH...");
    let ssh_transfer = SshTransfer::new(&ssh_dest)?;

    let src_root = Path::new(&args.source).parent().unwrap_or(&args.source);

    // Step 3: Transfer files
    println!("🚀 Starting SSH transfer ({} jobs)...", args.jobs);
    let m = Arc::new(MultiProgress::new());

    let semaphore = Arc::new(Semaphore::new(args.jobs));
    let mut handles = vec![];
    let ssh_transfer = Arc::new(ssh_transfer);
    let walker = walkdir::WalkDir::new(&args.source);
    walker.into_iter().filter_map(Result::ok).for_each(|entry| {
        let path = entry.path();
        if path.is_file() {
            println!("processing file : {}, {}, {}", src_root.display(),  remote_root.display(), path.display());
            let size = path.metadata().unwrap().len();
            let src_root = src_root.to_path_buf();
            let remote_root = remote_root.to_path_buf();
            let path = path.strip_prefix(&src_root).unwrap().to_path_buf();
            let sem = semaphore.clone();
            let ssh_transfer = ssh_transfer.clone();
            let m = m.clone();
            let h = tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let pb = m.add(ProgressBar::new(size));
                let sty = ProgressStyle::with_template("{msg} {bar:40} {bytes}/{total_bytes} ({eta})")
                    .unwrap()
                    .progress_chars("=>-");
                pb.set_style(sty);
                pb.set_message(utils::align_str(path.to_str().unwrap(), 20));
                // Send via SSH
                let _ = ssh_transfer.send_file(src_root, remote_root, path, size, pb);
            });

            handles.push(h);
        }
    });

    // Wait for all transfers
    for h in handles {
        let _ = h.await;
    }

    println!("✅ SSH transfer completed!");
    Ok(())
}

// Helper function to parse SSH destination
fn parse_ssh_destination(destination: &str) -> anyhow::Result<(String, String)> {
    // Format: user@host:path
    let dest_parts: Vec<&str> = destination.split(":").collect();
    if dest_parts.len() == 2 {
        return Ok((dest_parts[0].to_string(), dest_parts[1].to_string()));
    }else{
        Err(anyhow::anyhow!("Invalid SSH destination format. Expected user@host:path"))
    }
}