use clap::Parser;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Semaphore;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};

mod ssh;
use ssh::SshTransfer;

const PARALLELISM: usize = 8;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Source directory or files
    source: Vec<PathBuf>,

    /// Destination in format user@host:path or local/path
    destination: String,

    /// Enable compression for compressible files
    #[arg(short, long)]
    compress: bool,

    /// Number of parallel workers
    #[arg(short, long, default_value_t = PARALLELISM)]
    jobs: usize,

    /// Use SSH for remote transfer
    #[arg(long)]
    ssh: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct FileInfo {
    path: String,
    size: u64,
    mtime: u64,
    hash: String,
    compressed: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Manifest {
    files: Vec<FileInfo>,
}

async fn collect_metadata(sources: &[PathBuf]) -> anyhow::Result<Manifest> {
    let mut files = Vec::new();

    for src in sources {
        let walker = walkdir::WalkDir::new(src).into_iter();
        for entry in walker.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_file() {
                let metadata = path.metadata()?;
                let mtime = metadata.modified()?.duration_since(std::time::UNIX_EPOCH)?.as_secs();
                let size = metadata.len();

                // Read and hash first 4KB for performance
                let mut file = File::open(path)?;
                let mut buffer = vec![0; std::cmp::min(size, 4096) as usize];
                file.read_exact(&mut buffer)?;

                let mut hasher = Sha256::new();
                hasher.update(&buffer);
                if size > 4096 {
                    // For large files, also hash last 4KB
                    file.seek(std::io::SeekFrom::End(-4096))?;
                    let mut tail = [0u8; 4096];
                    file.read_exact(&mut tail)?;
                    hasher.update(&tail);
                }
                let hash = format!("{:x}", hasher.finalize());

                let path_str = path.strip_prefix(src.parent().unwrap_or(src))?.to_str().unwrap().to_string();
                let compressed = should_compress(&path_str);

                files.push(FileInfo {
                    path: path_str,
                    size,
                    mtime,
                    hash,
                    compressed,
                });
            }
        }
    }

    Ok(Manifest { files })
}

fn should_compress(path: &str) -> bool {
    let ext = Path::new(path).extension().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
    matches!(
        ext.as_str(),
        "txt" | "log" | "csv" | "json" | "xml" | "html" | "css" | "js" | "yaml" | "yml" | "md" | "toml"
    )
}

#[derive(Serialize, Deserialize)]
struct DataHeader {
    path: String,
    size: u64,
    mtime: u64,
    compressed: bool,
}

#[derive(Serialize, Deserialize)]
struct DataFooter {
    crc32: u32,
    sha256: String,
}

async fn send_file(
    src_root: &Path,
    dest_root: &Path,
    file_info: &FileInfo,
    pb: ProgressBar
) -> anyhow::Result<()> {
    let src_path = src_root.join(&file_info.path);
    let dest_path = dest_root.join(&file_info.path);
    fs::create_dir_all(dest_path.parent().unwrap())?;

    let mut input = BufReader::new(File::open(&src_path)?);
    let mut output = BufWriter::new(File::create(&dest_path)?);

    let header = DataHeader {
        path: file_info.path.clone(),
        size: file_info.size,
        mtime: file_info.mtime,
        compressed: file_info.compressed,
    };
    let header_bytes = serde_json::to_vec(&header)?;
    output.write_all(&(header_bytes.len() as u32).to_be_bytes())?;
    output.write_all(&header_bytes)?;

    let mut crc32_hasher = crc32fast::Hasher::new();
    let mut sha256_hasher = Sha256::new();

    let mut buffer = vec![0; 8192];
    let mut written = 0u64;

    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }

        let data = &buffer[..n];
        crc32_hasher.update(data);
        sha256_hasher.update(data);

        if file_info.compressed {
            let mut encoder = zstd::Encoder::new(Vec::new(), 3)?;
            encoder.write_all(data)?;
            let compressed_data = encoder.finish()?;
            output.write_all(&compressed_data)?;
            written += compressed_data.len() as u64;
        } else {
            output.write_all(data)?;
            written += n as u64;
        }

        pb.set_position(written);
    }

    let crc32 = crc32_hasher.finalize();
    let sha256 = format!("{:x}", sha256_hasher.finalize());

    let footer = DataFooter { crc32, sha256 };
    let footer_bytes = serde_json::to_vec(&footer)?;
    output.write_all(&(footer_bytes.len() as u32).to_be_bytes())?;
    output.write_all(&footer_bytes)?;

    pb.finish_with_message("done");
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    if args.ssh {
        handle_ssh_transfer(args).await?;
    } else {
        handle_local_transfer(args).await?;
    }
    
    Ok(())
}

async fn handle_local_transfer(args: Args) -> anyhow::Result<()> {
    let src_root = Path::new(&args.source[0]).parent().unwrap_or(&args.source[0]);
    let dest_root = Path::new(&args.destination);

    // Step 1: Collect metadata
    println!("🔍 Collecting metadata...");
    let manifest = collect_metadata(&args.source).await?;
    let total_size: u64 = manifest.files.iter().map(|f| f.size).sum();

    // Step 2: Create destination structure
    println!("📁 Creating destination structure...");
    for file in &manifest.files {
        let path = dest_root.join(&file.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
    }

    // Step 3: Parallel transfer
    println!("🚀 Starting parallel transfer ({} jobs)...", args.jobs);
    let m = MultiProgress::new();
    let sty = ProgressStyle::default_bar()
        .template("{msg} {bar:40} {bytes}/{total_bytes} ({eta})")?;

    let semaphore = Arc::new(Semaphore::new(args.jobs));
    let mut handles = vec![];

    for file in manifest.files {
        let pb = m.add(ProgressBar::new(file.size));
        pb.set_style(sty.clone());
        pb.set_message(file.path.clone());

        let src_root = src_root.to_path_buf();
        let dest_root = dest_root.to_path_buf();
        let file_info = file;
        let sem = semaphore.clone();

        let h = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let _ = send_file(&src_root, &dest_root, &file_info, pb).await;
        });

        handles.push(h);
    }

    // Wait for all transfers
    for h in handles {
        let _ = h.await;
    }

    println!("✅ Transfer completed!");
    Ok(())
}

async fn handle_ssh_transfer(args: Args) -> anyhow::Result<()> {
    // Parse destination
    let (_ssh_dest, remote_path) = parse_ssh_destination(&args.destination)?;
    let remote_root = Path::new(&remote_path);

    // Connect via SSH
    println!("🔗 Connecting via SSH...");
    let mut ssh_transfer = SshTransfer::new(&args.destination)?;

    let src_root = Path::new(&args.source[0]).parent().unwrap_or(&args.source[0]);

    // Step 1: Collect metadata
    println!("🔍 Collecting metadata...");
    let manifest = collect_metadata(&args.source).await?;

    // Step 2: Create remote directory structure
    println!("📁 Creating remote directory structure...");
    for file in &manifest.files {
        if let Some(parent) = remote_root.join(&file.path).parent() {
            let _ = ssh_transfer.create_remote_dir(parent.to_str().unwrap());
        }
    }

    // Step 3: Transfer files
    println!("🚀 Starting SSH transfer ({} jobs)...", args.jobs);
    let m = MultiProgress::new();
    let sty = ProgressStyle::default_bar()
        .template("{msg} {bar:40} {bytes}/{total_bytes} ({eta})")?;

    let semaphore = Arc::new(Semaphore::new(args.jobs));
    let mut handles = vec![];
    let ssh_transfer = Arc::new(ssh_transfer);

    for file in manifest.files {
        let pb = m.add(ProgressBar::new(file.size));
        pb.set_style(sty.clone());
        pb.set_message(file.path.clone());

        let src_path = src_root.join(&file.path);
        let remote_path = remote_root.join(&file.path);
        let sem = semaphore.clone();
        let ssh_transfer = ssh_transfer.clone();

        let h = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            
            // Read file data
            let mut file_data = Vec::new();
            let mut input_file = match File::open(&src_path) {
                Ok(file) => file,
                Err(_) => {
                    pb.finish_with_message("failed to open");
                    return;
                }
            };
            
            match input_file.read_to_end(&mut file_data) {
                Ok(_) => {},
                Err(_) => {
                    pb.finish_with_message("failed to read");
                    return;
                }
            };
            
            // Send via SSH
            let result = ssh_transfer.send_file_data(remote_path.to_str().unwrap(), &file_data);
            if result.is_ok() {
                pb.finish_with_message("done");
            } else {
                pb.finish_with_message("failed");
            }
        });

        handles.push(h);
    }

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
    if let Some(at_pos) = destination.find('@') {
        if let Some(colon_pos) = destination.find(':') {
            if colon_pos > at_pos {
                let ssh_part = destination[..colon_pos].to_string();
                let path_part = destination[colon_pos + 1..].to_string();
                return Ok((ssh_part, path_part));
            }
        }
    }
    
    Err(anyhow::anyhow!("Invalid SSH destination format. Expected user@host:path"))
}