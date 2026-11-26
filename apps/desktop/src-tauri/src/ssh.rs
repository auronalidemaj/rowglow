//! SSH tunnels: reach a database through an SSH server ("bastion").
//!
//! Opens an SSH session, listens on a random 127.0.0.1 port and forwards every
//! connection to `target_host:target_port` as seen from the SSH server
//! (direct-tcpip). The database driver then connects to the local port.
//!
//! Host keys are verified trust-on-first-use: an unknown server is refused
//! with its SHA256 fingerprint so the user can confirm it, and a server whose
//! key changed is refused outright (possible man-in-the-middle).

use crate::error::{AppError, AppResult};
use russh::client;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::timeout;

#[derive(Clone, Debug)]
pub enum SshAuth {
    Password(String),
    Key { path: String, passphrase: Option<String> },
}

#[derive(Clone, Debug)]
pub struct SshParams {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
    /// Fingerprint the user trusted earlier for this host:port, if any.
    pub known_fingerprint: Option<String>,
}

struct HostKeyCheck {
    expected: Option<String>,
    seen: Arc<Mutex<Option<String>>>,
}

fn fingerprint(key: &PublicKeyOrCertificate) -> String {
    match key {
        PublicKeyOrCertificate::PublicKey { key, .. } => key.fingerprint(HashAlg::Sha256).to_string(),
        PublicKeyOrCertificate::Certificate(cert) => {
            russh::keys::PublicKey::from(cert.public_key().clone()).fingerprint(HashAlg::Sha256).to_string()
        }
    }
}

impl client::Handler for HostKeyCheck {
    type Error = russh::Error;

    async fn check_server_key(&mut self, server_public_key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let fp = fingerprint(server_public_key);
        if let Ok(mut s) = self.seen.lock() {
            *s = Some(fp.clone());
        }
        Ok(self.expected.as_deref() == Some(fp.as_str()))
    }
}

/// A running tunnel. Dropping it closes the listener and the SSH session.
pub struct Tunnel {
    pub local_port: u16,
    task: tokio::task::JoinHandle<()>,
    handle: Arc<client::Handle<HostKeyCheck>>,
}

impl Tunnel {
    pub fn is_alive(&self) -> bool {
        !self.task.is_finished() && !self.handle.is_closed()
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn unknown_host_error(host: &str, port: u16, fingerprint: &str) -> AppError {
    let mut e = AppError::new(
        "connection",
        format!("First connection to the SSH server {host}:{port}. Confirm its host key fingerprint to continue."),
    )
    .with_code("ssh_unknown_host");
    e.detail = Some(fingerprint.to_string());
    e
}

pub async fn open_tunnel(p: &SshParams, target_host: &str, target_port: u16) -> AppResult<Tunnel> {
    if p.host.trim().is_empty() || p.user.trim().is_empty() {
        return Err(AppError::invalid("Enter the SSH host and user"));
    }
    let config = Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 4,
        ..Default::default()
    });
    let seen = Arc::new(Mutex::new(None));
    let check = HostKeyCheck {
        expected: p.known_fingerprint.clone(),
        seen: seen.clone(),
    };
    let port = if p.port == 0 { 22 } else { p.port };
    let connected = timeout(Duration::from_secs(15), client::connect(config, (p.host.trim(), port), check)).await;
    let seen_fp = seen.lock().ok().and_then(|s| s.clone());
    let mut handle = match connected {
        Err(_) => return Err(AppError::new("connection", format!("Timed out connecting to the SSH server {}:{port}", p.host))),
        Ok(Ok(h)) => h,
        Ok(Err(e)) => {
            return Err(match (&p.known_fingerprint, seen_fp) {
                (None, Some(fp)) => unknown_host_error(p.host.trim(), port, &fp),
                (Some(known), Some(fp)) if *known != fp => {
                    let mut err = AppError::new(
                        "connection",
                        format!(
                            "The SSH server {}:{port} presented a different host key than before. This can mean someone is intercepting the connection, so Rowglow refused to connect. If the server was legitimately reinstalled, forget its key in the connection settings.",
                            p.host
                        ),
                    )
                    .with_code("ssh_host_key_changed");
                    err.detail = Some(fp);
                    err
                }
                _ => AppError::new("connection", format!("SSH connection to {}:{port} failed: {e}", p.host)),
            });
        }
    };

    let ok = match &p.auth {
        SshAuth::Password(pw) => handle
            .authenticate_password(p.user.trim(), pw.clone())
            .await
            .map_err(|e| AppError::new("connection", format!("SSH login failed: {e}")))?
            .success(),
        SshAuth::Key { path, passphrase } => {
            let path = crate::drivers::sqlite::expand_path(path);
            let key = russh::keys::load_secret_key(&path, passphrase.as_deref()).map_err(|e| {
                AppError::new("connection", format!("Couldn't read the SSH key {}: {e}", path.display()))
            })?;
            let hash = handle.best_supported_rsa_hash().await.ok().flatten().flatten();
            handle
                .authenticate_publickey(p.user.trim(), PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await
                .map_err(|e| AppError::new("connection", format!("SSH login failed: {e}")))?
                .success()
        }
    };
    if !ok {
        return Err(AppError::new(
            "connection",
            format!("The SSH server rejected the login for {}@{}", p.user.trim(), p.host.trim()),
        )
        .with_code("ssh_auth_failed"));
    }

    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let local_port = listener.local_addr()?.port();
    let handle = Arc::new(handle);
    let h = handle.clone();
    let target_host = target_host.to_string();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut socket, peer)) = listener.accept().await else { break };
            let h = h.clone();
            let target = target_host.clone();
            tokio::spawn(async move {
                let channel = match h
                    .channel_open_direct_tcpip(target, target_port as u32, "127.0.0.1", peer.port() as u32)
                    .await
                {
                    Ok(c) => c,
                    Err(_) => return,
                };
                let mut stream = channel.into_stream();
                let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
            });
        }
    });
    Ok(Tunnel { local_port, task, handle })
}
