use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use tokio::{runtime::Builder, task::JoinHandle};
use std::path::{PathBuf, Path};

mod ssh;
mod utils;
mod local;

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

struct Destination {
    pub(crate) is_ssh: bool,
    //root@host
    pub(crate) ssh_part: String,
    ///path/xxx
    pub(crate) remote_path: String,
}

impl Destination {
    pub(crate) fn new(destination: &str) -> anyhow::Result<Self> {
        let dest_parts: Vec<&str> = destination.split(":").collect();
        if dest_parts.len() == 2 {
            return Ok(Self {
                is_ssh: true,
                ssh_part: dest_parts[0].to_string(),
                remote_path: dest_parts[1].to_string(),
            });
        }else if dest_parts.len() == 1{
            return Ok(Self {
                is_ssh: false,
                ssh_part: "".to_string(),
                remote_path: destination.to_string(),
            });
        }else{
            Err(anyhow::anyhow!("Invalid destination format. Expected user@host:path or local/path"))
        }
    }
}

//#[tokio::main]
fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let rt = Builder::new_multi_thread()
        .worker_threads(args.jobs) // 核心异步线程数
        .max_blocking_threads(args.jobs) // 设置阻塞线程池的最大线程数
        .enable_all()
        .build()
        .unwrap();

    let _ = rt.block_on(async {
        let r = process(&args).await;
        match r {
           Ok(_) => {},
           Err(e) => {eprintln!("Unexpected Error: {}", e);} 
        }
    });
    Ok(())
}


async fn process(args: &Args) -> anyhow::Result<()> { 
    // Parse destination
    let Destination{is_ssh, ssh_part, remote_path} = Destination::new(&args.destination)?;
    //目标路径
    let target_root = Path::new(&remote_path);
    //源路径
    let src_root = Path::new(&args.source).parent().unwrap_or(&args.source);
    println!("🚀 Start scanning files...");
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
    println!("🚀 A total of {} files to be transferred...", &files.len());
    
    // Create a single progress bar for all files
    let pb = ProgressBar::new(total_size);
    let sty = ProgressStyle::with_template("{msg} {bar:40} {bytes}/{total_bytes} ({eta})")
        .unwrap()
        .progress_chars("=>-");
    pb.set_style(sty);
    
    // Print transfer start message before starting the transfer tasks
    println!("🚀 Start transfering ({} jobs)...", args.jobs);
    
    let mut handles: Vec<JoinHandle<()>> = vec![];
    if is_ssh {
        ssh::copy_files(ssh_part, src_root, target_root, files, &pb, args.jobs, &mut handles)?;
    }else{
        local::copy_files(src_root, target_root, files, &pb, &mut handles);
    }
    
    for h in handles {
        let _ = h.await;
    }
    pb.finish_and_clear();
    println!("✅ Transfer done!");
    Ok(())
}