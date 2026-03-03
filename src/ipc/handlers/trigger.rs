// Author: Dustin Pilgrim
// License: MIT

use tokio::sync::mpsc;

use crate::core::events::Event;
use crate::core::manager_msg::ManagerMsg;

pub const TRIGGER_HELP_MESSAGE: &str = r#"Usage:
  stasis trigger <step>

Examples:
  stasis trigger all
  stasis trigger startup
  stasis trigger dpms
  stasis trigger early-dpms
  stasis trigger brightness
  stasis trigger lock_screen
  stasis trigger suspend
  stasis trigger custom:my-step

Notes:
  - This sends a request to the running daemon.
  - The daemon decides what “<step>” means for the active profile/plan.
  - `all` runs every enabled non-instant step in plan order (skips timeout_seconds==0 steps).
"#;

/// IPC handler: `trigger <name>`
///
/// For now this just forwards a daemon event; the daemon/Manager implements what names mean.
pub async fn handle_trigger(args: &str, tx: &mpsc::Sender<ManagerMsg>) -> String {
    let args = args.trim();

    if args.is_empty() || args.eq_ignore_ascii_case("help") || args == "-h" || args == "--help" {
        if args.is_empty() {
            return format!("ERROR: missing step name\n\n{TRIGGER_HELP_MESSAGE}");
        }
        return TRIGGER_HELP_MESSAGE.to_string();
    }

    // Support `trigger foo bar` by treating everything as the name (but typical is one token).
    let name = args.to_string();

    let now_ms = crate::core::utils::now_ms();

    if tx
        .send(ManagerMsg::Event(Event::ManualTrigger {
            now_ms,
            name: name.clone(),
        }))
        .await
        .is_err()
    {
        return "ERROR: daemon event channel closed".to_string();
    }

    format!("Triggered '{name}'")
}
