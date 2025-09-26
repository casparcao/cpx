use anyhow::Result;
use ssh2::Session;
use std::net::TcpStream;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::Mutex;
use std::collections::VecDeque;

pub struct SshConnectionPool {
    connections: Arc<Mutex<VecDeque<Session>>>,
    ssh_dest: String,
    max_connections: usize,
}

impl SshConnectionPool {
    pub fn new(ssh_dest: String, max_connections: usize) -> Result<Self> {
        let pool = SshConnectionPool {
            connections: Arc::new(Mutex::new(VecDeque::new())),
            ssh_dest,
            max_connections,
        };
        
        Ok(pool)
    }
    
     fn create_new_connection(&self) -> Result<Session> {
        // Further parse user@host into user and host
        let parts: Vec<&str> = self.ssh_dest.split('@').collect();
        let (user, host) = if parts.len() == 2 {
            (parts[0].to_string(), parts[1].to_string())
        } else {
            // Default to current user if no user specified
            (whoami::username(), self.ssh_dest.to_string())
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
                    unsafe{
                        env::set_var("SSH_PASSWORD", &password);
                    }
                    auth_success = true;
                }
            }
        }

        if !auth_success {
            return Err(anyhow::anyhow!("Unable to authenticate with SSH server. Please ensure you have set up SSH keys, ssh-agent, or provide a valid password."));
        }
        
        Ok(session)
    }
    
    pub  fn get_connection(&self) -> Result<Session> {
        {
            // Try to get an existing connection from the pool
            let mut connections = self.connections.lock().unwrap();
            while let Some(session) = connections.pop_front() {
                // Check if the session is still valid
                if session.authenticated() {
                    // Additional check to see if session is still responsive
                    match session.channel_session() {
                        Ok(mut channel) => {
                            // Close the test channel
                            let _ = channel.close();
                            let _ = channel.wait_close();
                            return Ok(session);
                        }
                        Err(e) => {
                            // Session is not responsive, continue to next session
                            eprintln!("Session is not responsive, continuing to next session. {}", e);
                            continue;
                        }
                    }
                }
            }
        }
        self.create_new_connection()
    }
    
    pub  fn return_connection(&self, session: Session) {
        let mut connections = self.connections.lock().unwrap();
        
        // Only return connection to pool if it's still valid and we're under the limit
        if session.authenticated() && connections.len() < self.max_connections {
            // Test if session is still responsive before returning to pool
            if let Ok(mut channel) = session.channel_session() {
                // Close the test channel
                let _ = channel.close();
                let _ = channel.wait_close();
                connections.push_back(session);
            }
        }
        // If the connection is not valid or we're at capacity, it will be dropped and cleaned up
    }
}



fn read_password() -> Result<String> {
    let password = rpassword::read_password()?;
    Ok(password)
}
