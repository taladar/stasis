// Author: Dustin Pilgrim
// License: GPL-3.0-only

use tokio::sync::{mpsc, oneshot};

use crate::core::manager_msg::ManagerMsg;

pub async fn handle_profile(args: &str, tx: &mpsc::Sender<ManagerMsg>) -> String {
    let args = args.trim();

    // Help passthrough
    if args.is_empty() || args == "help" || args == "-h" || args == "--help" {
        return profile_help();
    }

    // Only allow a single token for now
    let mut it = args.split_whitespace();
    let name = it.next().unwrap();
    if it.next().is_some() {
        return "ERROR: usage: stasis profile <name|none>".to_string();
    }

    let name_opt = if name == "none" {
        None
    } else {
        Some(name.to_string())
    };

    let (reply_tx, reply_rx) = oneshot::channel();

    if tx
        .send(ManagerMsg::SetProfile {
            name: name_opt,
            reply: reply_tx,
        })
        .await
        .is_err()
    {
        return "ERROR: Stasis daemon not running".to_string();
    }

    match reply_rx.await {
        Ok(Ok(msg)) => msg,
        Ok(Err(e)) => format!("ERROR: {e}"),
        Err(_) => "ERROR: No response from daemon".to_string(),
    }
}

fn profile_help() -> String {
    r#"Usage: stasis profile <name|none>

Switch the active profile used for config selection.

Examples:
  stasis profile desktop
  stasis profile laptop
  stasis profile none
"#
    .to_string()
}
