// Author: Dustin Pilgrim
// License: MIT

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    time::{Duration, timeout},
};

pub async fn send_raw(cmd: &str) -> Result<String, String> {
    let path = crate::ipc::socket_path()?;

    // Local fallback for `dump` (does not require daemon).
    async fn dump_fallback(cmd: &str) -> Option<String> {
        let trimmed = cmd.trim_start();
        let rest = trimmed.strip_prefix("dump")?;
        Some(crate::ipc::handlers::dump::handle_dump(rest).await)
    }

    // If socket file doesn't exist, allow dump to run offline.
    if !path.exists() {
        if let Some(out) = dump_fallback(cmd).await {
            return Ok(out);
        }
        return Err("daemon not running".to_string());
    }

    let mut stream = match timeout(Duration::from_secs(2), UnixStream::connect(&path)).await {
        Ok(Ok(s)) => s,
        Ok(Err(_e)) => {
            // Socket exists but nothing is listening (stale socket / crashed daemon).
            if let Some(out) = dump_fallback(cmd).await {
                return Ok(out);
            }
            return Err(format!(
                "failed to connect to {}: daemon not running",
                path.display()
            ));
        }
        Err(_) => {
            if let Some(out) = dump_fallback(cmd).await {
                return Ok(out);
            }
            return Err("timeout connecting to daemon".to_string());
        }
    };

    timeout(Duration::from_secs(2), stream.write_all(cmd.as_bytes()))
        .await
        .map_err(|_| "timeout writing to daemon".to_string())?
        .map_err(|e| format!("write failed: {e}"))?;

    timeout(Duration::from_secs(2), stream.shutdown())
        .await
        .map_err(|_| "timeout finalizing request".to_string())?
        .map_err(|e| format!("shutdown failed: {e}"))?;

    let mut resp = Vec::new();
    timeout(Duration::from_secs(2), stream.read_to_end(&mut resp))
        .await
        .map_err(|_| "timeout reading response".to_string())?
        .map_err(|e| format!("read failed: {e}"))?;

    Ok(String::from_utf8_lossy(&resp).to_string())
}
