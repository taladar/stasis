// Author: Dustin Pilgrim
// License: MIT

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
    sync::mpsc,
};

use crate::core::manager_msg::ManagerMsg;

pub async fn spawn_ipc_server(tx: mpsc::Sender<ManagerMsg>) -> Result<(), String> {
    let path = crate::ipc::socket_path()?;

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Remove stale socket file (if any). Ignore errors.
    let _ = std::fs::remove_file(&path);

    let listener = UnixListener::bind(&path)
        .map_err(|e| format!("failed to bind ipc socket {}: {e}", path.display()))?;

    eventline::info!("ipc: listening on {}", path.display());

    tokio::spawn(async move {
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(x) => x,
                Err(e) => {
                    eventline::error!("ipc: accept failed: {}", e);
                    continue;
                }
            };

            let tx = tx.clone();
            tokio::spawn(async move {
                // Read the whole request (client must shutdown its write-half)
                let mut buf = Vec::new();
                if let Err(e) = stream.read_to_end(&mut buf).await {
                    eventline::warn!("ipc: read failed: {}", e);
                    return;
                }

                let cmd = String::from_utf8_lossy(&buf).trim().to_string();
                if cmd.is_empty() {
                    let _ = stream.write_all(b"ERROR: empty command").await;
                    let _ = stream.shutdown().await; // close response
                    return;
                }

                eventline::debug!("ipc: command: {}", cmd);

                let response = crate::ipc::router::route_command(&cmd, &tx).await;

                if let Err(e) = stream.write_all(response.as_bytes()).await {
                    eventline::warn!("ipc: write failed: {}", e);
                    return;
                }

                let _ = stream.shutdown().await;
            });
        }
    });

    Ok(())
}
