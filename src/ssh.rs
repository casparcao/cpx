use anyhow::Result;
use ssh2::Session;
use std::io::prelude::*;
use std::net::TcpStream;
use std::path::Path;

pub struct SshTransfer {
    session: Session,
    host: String,
    path: String,
}

impl SshTransfer {
    pub fn new(destination: &str) -> Result<Self> {
        // Parse destination in format user@host:path
        let (ssh_dest, path) = parse_ssh_destination(destination)?;
        
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

        // Try to authenticate with ssh-agent first
        if session.userauth_agent(&user).is_err() {
            // If ssh-agent fails, try with a password (will need to be provided)
            // For now, we'll return an error - in a real implementation you might
            // want to prompt for a password or support key files
            return Err(anyhow::anyhow!("Unable to authenticate with SSH agent. Manual authentication not yet implemented."));
        }

        Ok(SshTransfer {
            session,
            host,
            path,
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

fn parse_ssh_destination(destination: &str) -> Result<(String, String)> {
    // Format: user@host:path
    if let Some(at_pos) = destination.find('@') {
        if let Some(colon_pos) = destination.find(':') {
            if colon_pos > at_pos {
                let ssh_part = destination[..colon_pos].to_string();
                let path_part = destination[colon_pos + 1..].to_string();
                return Ok((ssh_part, path_part));
            }
        }
    } else if let Some(colon_pos) = destination.find(':') {
        // Format: host:path (no user specified)
        let host_part = destination[..colon_pos].to_string();
        let path_part = destination[colon_pos + 1..].to_string();
        return Ok((host_part, path_part));
    }
    
    Err(anyhow::anyhow!("Invalid SSH destination format. Expected user@host:path or host:path"))
}