// Author: Dustin Pilgrim
// License: MIT

use std::collections::VecDeque;
use std::path::PathBuf;

const DEFAULT_LINES: usize = 100;
const MAX_LINES: usize = 2000;

pub async fn handle_dump(args: &str) -> String {
    let args = args.trim();

    if args == "help" || args == "--help" || args == "-h" {
        return dump_help();
    }

    let n = match parse_lines_arg(args) {
        Ok(n) => n,
        Err(e) => return e,
    };

    let path = match log_path() {
        Some(p) => p,
        None => return "ERROR: HOME not set; cannot locate log file".to_string(),
    };

    let data = match tokio::fs::read_to_string(&path).await {
        Ok(s) => s,
        Err(e) => {
            return format!(
                "ERROR: failed to read log {}: {e}",
                path.display()
            );
        }
    };

    // Keep only the last N lines (bounded memory)
    let mut buf: VecDeque<&str> = VecDeque::with_capacity(n);
    for line in data.lines() {
        if buf.len() == n {
            buf.pop_front();
        }
        buf.push_back(line);
    }

    if buf.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    for line in buf {
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn parse_lines_arg(args: &str) -> Result<usize, String> {
    if args.is_empty() {
        return Ok(DEFAULT_LINES);
    }

    let mut it = args.split_whitespace();
    let first = it.next().unwrap();

    if it.next().is_some() {
        return Err("ERROR: usage: stasis dump [N]".to_string());
    }

    let n: usize = first
        .parse()
        .map_err(|_| "ERROR: N must be a positive integer".to_string())?;

    if n == 0 {
        return Err("ERROR: N must be >= 1".to_string());
    }

    Ok(n.min(MAX_LINES))
}

fn dump_help() -> String {
    r#"Usage: stasis dump [N]

Print the last N lines of the Stasis log.

Arguments:
  N        Number of lines to print (default: 100, max: 2000)

Examples:
  stasis dump
  stasis dump 50
"#
    .to_string()
}

fn log_path() -> Option<PathBuf> {
    // Prefer XDG_STATE_HOME per spec
    let base = if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
        PathBuf::from(state)
    } else {
        let home = std::env::var_os("HOME")?;
        PathBuf::from(home).join(".local").join("state")
    };

    Some(base.join("stasis").join("stasis.log"))
}

