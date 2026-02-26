// Author: Dustin Pilgrim
// License: MIT

use tokio::sync::{mpsc, oneshot};

use crate::core::manager_msg::{ListKind, ManagerMsg};

pub const LIST_HELP_MESSAGE: &str = r#"Usage:
  stasis list actions
  stasis list profiles

Notes:
  - `actions` shows the currently effective plan (profile + power source).
  - `profiles` shows all configured profile names you can switch to.
"#;

pub async fn handle_list(args: &str, tx: &mpsc::Sender<ManagerMsg>) -> String {
    let a = args.trim();

    if a.is_empty() || a.eq_ignore_ascii_case("help") || a == "-h" || a == "--help" {
        return LIST_HELP_MESSAGE.to_string();
    }

    let sub = a.split_whitespace().next().unwrap_or("");

    let kind = match sub {
        "actions" => ListKind::Actions,
        "profiles" => ListKind::Profiles,
        "help" | "-h" | "--help" => return LIST_HELP_MESSAGE.to_string(),
        _ => return format!("ERROR: unknown list subcommand '{sub}'\n\n{LIST_HELP_MESSAGE}"),
    };

    let (reply_tx, reply_rx) = oneshot::channel();

    if tx
        .send(ManagerMsg::List { kind, reply: reply_tx })
        .await
        .is_err()
    {
        return "ERROR: daemon event channel closed".to_string();
    }

    match reply_rx.await {
        Ok(Ok(mut s)) => {
            if !s.ends_with('\n') {
                s.push('\n');
            }
            s
        }
        Ok(Err(e)) => format!("ERROR: {e}\n"),
        Err(_) => "ERROR: daemon list channel closed\n".to_string(),
    }
}
