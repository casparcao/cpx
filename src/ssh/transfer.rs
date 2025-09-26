use anyhow::Result;
use indicatif::ProgressBar;
use ssh2::Session;
use std::fs::File;
use std::io::prelude::*;
use std::io::BufReader;
use std::path::Path;
use std::path::PathBuf;
use std::io::Write;


pub  fn send_file(
    session: &Session,
    src_root: PathBuf,
    dest_root: PathBuf,
    path: PathBuf,
    size: u64,
    pb: ProgressBar) -> Result<()> {
    // Create full remote path
    let remote_path = dest_root.join(&path);
    create_remote_dir(&session, &dest_root.join(&path).parent().unwrap_or(&dest_root).to_str().unwrap())?;

    let mut input = BufReader::new(File::open(&src_root.join(&path))?);
    let mut buffer = vec![0; 8192];
    pb.set_message(crate::utils::align_str(&path.to_string_lossy(), 20));

    // Use SCP to send file data
    let mut channel = session.scp_send(
        Path::new(&remote_path), 
        0o644, 
        size, 
        None
    )?;

    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        let data = &buffer[..n];
        channel.write_all(data)?;
        pb.inc(n as u64);
    }
    channel.send_eof()?;
    channel.wait_eof()?;
    channel.close()?;
    channel.wait_close()?;
    Ok(())
}

fn create_remote_dir(session: &Session, remote_path: &str) -> Result<()> {
    // Execute mkdir command to create directory
    let mut channel = session.channel_session()?;
    channel.exec(&format!("mkdir -p {}", remote_path))?;
    channel.send_eof()?;
    channel.wait_eof()?;
    channel.close()?;
    channel.wait_close()?;
    Ok(())
}