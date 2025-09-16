use clap::Parser;
use tokio::runtime::Builder;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Semaphore;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

mod ssh;
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
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        let data = &buffer[..n];
        output.write_all(data)?;
        pb.inc(n as u64);
    }
    Ok(())
}

//#[tokio::main]
fn main() -> anyhow::Result<()> {
    let rt = Builder::new_multi_thread()
        .worker_threads(4) // 核心异步线程数
        .max_blocking_threads(4) // 设置阻塞线程池的最大线程数
        .enable_all()
        .build()
        .unwrap();

    let _ = rt.block_on(async {
        let args = Args::parse();

        let dest_parts = args.destination.split(":").collect::<Vec<_>>();

        if dest_parts.len() == 2 {
            cp_ssh_files(args).await
        } else if dest_parts.len() == 1 {
            cp_local_files(args).await
        } else {
            anyhow::bail!("Invalid destination format");
        }
    });

    Ok(())
}

async fn cp_local_files(args: Args) -> anyhow::Result<()> {
    let src_root = Path::new(&args.source).parent().unwrap_or(&args.source);
    let dest_root = Path::new(&args.destination);
    println!("Copying from {} to {}", src_root.display(), dest_root.display());
    
    // Calculate total size for progress bar
    let mut total_size = 0u64;
    let mut files = Vec::new();
    let walker = walkdir::WalkDir::new(&args.source);
    walker.into_iter().filter_map(Result::ok).for_each(|entry| {
        let path = entry.path();
        if path.is_file() {
            let size = path.metadata().unwrap().len();
            // 只处理非空文件
            if size > 0 {
                total_size += size;
                let path = path.strip_prefix(&src_root).unwrap().to_path_buf();
                files.push((path, size));
            }
        }
    });

    // Create a single progress bar for all files
    let pb = ProgressBar::new(total_size);
    let sty = ProgressStyle::with_template("{bar:40} {bytes}/{total_bytes} ({eta})")
        .unwrap()
        .progress_chars("=>-");
    pb.set_style(sty);

    let semaphore = Arc::new(Semaphore::new(args.jobs));
    let mut handles = vec![];

    for (path, size) in files {
        let src_root = src_root.to_path_buf();
        let dest_root = dest_root.to_path_buf();
        let sem = semaphore.clone();
        let pb = pb.clone();

        let h = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let _ = send_file(src_root, dest_root, path, pb.clone()).await;
        });
        handles.push(h);
    }

    // Wait for all transfers
    for h in handles {
        let _ = h.await;
    }
    
    pb.finish_and_clear();
    println!("✅ Transfer completed!");
    Ok(())
}

async fn cp_ssh_files(args: Args) -> anyhow::Result<()> {
    // Parse destination
    let (ssh_dest, remote_path) = parse_ssh_destination(&args.destination)?;
    let remote_root = Path::new(&remote_path);

    let src_root = Path::new(&args.source).parent().unwrap_or(&args.source);

    // Create SSH connection pool
    println!("🔗 Creating SSH connection pool...");
    let connection_pool = Arc::new(ssh::SshConnectionPool::new(ssh_dest, args.jobs)?);

    // Step 3: Transfer files
    println!("🚀 Starting SSH transfer ({} jobs)...", args.jobs);
    
    // Calculate total size for progress bar
    let mut total_size = 0u64;
    let mut files = Vec::new();
    let walker = walkdir::WalkDir::new(&args.source);
    walker.into_iter().filter_map(Result::ok).for_each(|entry| {
        let path = entry.path();
        if path.is_file() {
            let size = path.metadata().unwrap().len();
            // 只处理非空文件
            if size > 0 {
                total_size += size;
                let path = path.strip_prefix(&src_root).unwrap().to_path_buf();
                files.push((path, size));
            }
        }
    });

    // Create a single progress bar for all files
    let pb = ProgressBar::new(total_size);
    let sty = ProgressStyle::with_template("{bar:40} {bytes}/{total_bytes} ({eta})")
        .unwrap()
        .progress_chars("=>-");
    pb.set_style(sty);

    let mut handles = vec![];
    for (path, size) in files {
        let src_root = src_root.to_path_buf();
        let remote_root = remote_root.to_path_buf();
        let pool = connection_pool.clone();
        let pb = pb.clone();
        
        let h = tokio::task::spawn_blocking(move || {
            // Try to get connection from pool with retry logic
            let ssh_session = loop {
                match pool.get_connection(){
                    Ok(session) => break session,
                    Err(e) => {
                        eprintln!("Failed to get SSH connection from pool: {}. Retrying in 1 second...", e);
                        std::thread::sleep(tokio::time::Duration::from_secs(1));
                    }
                }
            };
            
            // Wrap session in SshTransfer for compatibility
            let ssh_transfer = ssh::SshTransfer::from_session(ssh_session);
            
            // Send via SSH
            let r = ssh_transfer.send_file(src_root, remote_root, path, size, pb.clone());
            
            // Return connection to pool
            pool.return_connection(ssh_transfer.into_session());
            
            match r {
                Ok(_) => {},
                Err(e) => {
                    eprintln!("Error: {}", e);
                    // 即使出错也要更新进度条
                    pb.inc(size);
                }
            }
        });
        handles.push(h);
    }

    println!("🚀 Starting SSH transfer ({} jobs)...{}", args.jobs, handles.len());
    // Wait for all transfers
    for h in handles {
        let _ = h.await;
    }
    
    pb.finish_and_clear();
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