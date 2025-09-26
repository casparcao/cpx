use std::{fs::{self, File}, io::{BufReader, BufWriter, Read, Write}, path::{Path, PathBuf}};
use indicatif::ProgressBar;



pub(crate) fn copy_files(src_root: &Path, 
    target_root: &Path, 
    files: Vec<(PathBuf, u64)>, 
    pb: &ProgressBar, 
    handles: &mut Vec<tokio::task::JoinHandle<()>>) {
    for (path, size) in files {
        let src_root = src_root.to_path_buf();
        let dest_root = target_root.to_path_buf();
        let pb = pb.clone();

        let h = tokio::spawn(async move {
            let r = send_file(src_root, dest_root, path, pb.clone()).await;

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
}


pub(crate) async fn send_file(
    src_root: PathBuf,
    target_root: PathBuf,
    path: PathBuf,
    pb: ProgressBar
) -> anyhow::Result<()> {
    let src_path = src_root.join(&path);
    let dest_path = target_root.join(&path);
    fs::create_dir_all(dest_path.parent().unwrap())?;
    let mut input = BufReader::new(File::open(&src_path)?);
    let mut output = BufWriter::new(File::create(&dest_path)?);
    let mut buffer = vec![0; 8192];
    pb.set_message(crate::utils::align_str(&path.to_string_lossy(), 20));
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