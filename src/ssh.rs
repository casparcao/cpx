use anyhow::Result;
use indicatif::ProgressBar;
use ssh2::Session;
use std::fs::File;
use std::io::prelude::*;
use std::io::BufReader;
use std::net::TcpStream;
use std::path::Path;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::io::{self, Write};

pub struct SshTransfer {
    session: Session,
}

impl SshTransfer {
    pub fn new(ssh_dest: &str) -> Result<Self> {
        // Further parse user@host into user and host
        let parts: Vec<&str> = ssh_dest.split('@').collect();
        let (user, host) = if parts.len() == 2 {
            (parts[0].to_string(), parts[1].to_string())
        } else {
            // Default to current user if no user specified
            (whoami::username(), ssh_dest.to_string())
        };

        // Connect to SSH server (assuming default SSH port 22)
        let tcp = TcpStream::connect(&(host.as_str(), 22))?;
        let mut session = Session::new()?;
        session.set_tcp_stream(tcp);
        session.handshake()?;

        // Try various authentication methods in order of preference
        let mut auth_success = false;
        
        // 1. Try ssh-agent authentication first
        if session.userauth_agent(&user).is_ok() {
            auth_success = true;
        }
        
        // 2. Try public key authentication
        if !auth_success {
            if let Ok(home_dir) = env::var("HOME")
                    .or_else(|_err| env::var("USERPROFILE")) {
                let mut ssh_path = PathBuf::new();
                ssh_path.push(home_dir);
                ssh_path.push(".ssh");
            
                let pub_key_path = ssh_path.join("id_rsa.pub");
                let priv_key_path = ssh_path.join("id_rsa");
                println!("Using public key authentication with keys at {} and {}", pub_key_path.display(), priv_key_path.display());
                
                if fs::metadata(&pub_key_path).is_ok() && fs::metadata(&priv_key_path).is_ok() {
                    // Try to authenticate with default RSA keys
                    if session.userauth_pubkey_file(&user, Some(&pub_key_path), &priv_key_path, None).is_ok() {
                        auth_success = true;
                    }
                }
            }
        }
        
        // 3. Try password authentication
        if !auth_success {
            // Try to get password from environment variable first
            if let Ok(password) = env::var("SSH_PASSWORD") {
                if session.userauth_password(&user, &password).is_ok() {
                    auth_success = true;
                }
            }
            
            // If environment variable not set or authentication failed, prompt user for password
            if !auth_success {
                print!("Password for {}@{}: ", user, host);
                io::stdout().flush()?;
                let password = read_password()?;
                if session.userauth_password(&user, &password).is_ok() {
                    auth_success = true;
                }
            }
        }

        if !auth_success {
            return Err(anyhow::anyhow!("Unable to authenticate with SSH server. Please ensure you have set up SSH keys, ssh-agent, or provide a valid password."));
        }

        Ok(SshTransfer {
            session,
        })
    }


    pub async fn send_file(
        &self,
        src_root: PathBuf,
        dest_root: PathBuf,
        path: PathBuf,
        size: u64,
        pb: ProgressBar) -> Result<()> {
        // Create full remote path
        println!("Sending file: {}, {}, {}", src_root.display(), dest_root.display(), path.display());
        let remote_path = dest_root.join(&path);
        self.create_remote_dir(&dest_root.join(&path).parent().unwrap_or(&dest_root).to_str().unwrap())?;

        let mut input = BufReader::new(File::open(&src_root.join(&path))?);
        let mut buffer = vec![0; 8192];
        let mut written = 0u64;

        // Use SCP to send file data
        let mut channel = self.session.scp_send(
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
            written += n as u64;
            pb.set_position(written);
        }
        channel.send_eof()?;
        channel.wait_eof()?;
        channel.close()?;
        channel.wait_close()?;
        pb.finish_and_clear();
        Ok(())
    }

    pub fn create_remote_dir(&self, remote_path: &str) -> Result<()> {
        // Execute mkdir command to create directory
        let mut channel = self.session.channel_session()?;
        channel.exec(&format!("mkdir -p {}", remote_path))?;
        channel.send_eof()?;
        channel.wait_eof()?;
        channel.close()?;
        channel.wait_close()?;
        Ok(())
    }

}

fn read_password() -> Result<String> {
    let password = rpassword::read_password()?;
    Ok(password)
}
