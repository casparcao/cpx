use anyhow::Result;
use ssh2::Session;
use std::io::prelude::*;
use std::net::TcpStream;
use std::path::Path;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::io::{self, Write};

pub struct SshTransfer {
    session: Session,
    host: String,
    path: String,
}

impl SshTransfer {
    pub fn new((ssh_dest, path): (&str, &str)) -> Result<Self> {
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
            host,
            path: path.to_string(),
        })
    }


    pub fn send_file_data(&self, remote_path: &str, data: &[u8]) -> Result<()> {
        // Create full remote path
        let full_remote_path = if self.path.ends_with('/') {
            format!("{}{}", self.path, remote_path)
        } else {
            format!("{}/{}", self.path, remote_path)
        };

        // Create directory if needed
        let remote_dir = Path::new(&full_remote_path).parent()
            .ok_or_else(|| anyhow::anyhow!("Invalid remote path"))?
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid UTF-8 in remote path"))?;

        self.create_remote_dir(remote_dir)?;

        // Use SCP to send file data
        let mut channel = self.session.scp_send(
            Path::new(&full_remote_path), 
            0o644, 
            data.len() as u64, 
            None
        )?;
        
        channel.write_all(data)?;
        channel.send_eof()?;
        channel.wait_eof()?;
        channel.close()?;
        channel.wait_close()?;

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

    pub fn execute_command(&self, command: &str) -> Result<String> {
        let mut channel = self.session.channel_session()?;
        channel.exec(command)?;
        
        let mut output = String::new();
        channel.read_to_string(&mut output)?;
        channel.send_eof()?;
        channel.wait_eof()?;
        channel.close()?;
        channel.wait_close()?;
        
        Ok(output)
    }
}

fn read_password() -> Result<String> {
    let password = rpassword::read_password()?;
    Ok(password)
}
