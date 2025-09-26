use std::path::{Path, PathBuf};
use std::sync::Arc;
use indicatif::ProgressBar;


mod pool;
pub(crate) mod transfer;


pub(crate) fn copy_files(
    ssh_part: String,
    src_root: &Path,
    target_root: &Path,  
    files: Vec<(PathBuf, u64)>, 
    pb: &ProgressBar, 
    jobs: usize,
    handles: &mut Vec<tokio::task::JoinHandle<()>>) -> anyhow::Result<()> {
    let connection_pool = Arc::new(pool::SshConnectionPool::new(ssh_part, jobs)?);
    for (path, size) in files {
        let src_root = src_root.to_path_buf();
        let target_root = target_root.to_path_buf();
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
        
            // Send via SSH
            let r = transfer::send_file(&ssh_session, src_root, target_root, path, size, pb.clone());
        
            // Return connection to pool
            pool.return_connection(ssh_session);
        
            match r {
                Ok(_) => {},
                Err(e) => {
                    eprintln!("Unexpected Error: {}", e);
                    // 即使出错也要更新进度条
                    pb.inc(size);
                }
            }
        });
        handles.push(h);
    }
    Ok(())
}
