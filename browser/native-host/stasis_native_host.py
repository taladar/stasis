#!/usr/bin/env python3
import json
import os
import socket
import struct
import sys
import time
from typing import Any, Dict, Optional


def log_line(msg: str) -> None:
    try:
        path = os.environ.get(
            "STASIS_BROWSER_HOST_LOG",
            os.path.expanduser("~/.local/state/stasis/browser-host.log"),
        )
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "a", encoding="utf-8") as fh:
            fh.write(f"{int(time.time())} {msg}\n")
    except Exception:
        pass


def stasis_socket_path() -> str:
    xdg_runtime_dir = os.environ.get("XDG_RUNTIME_DIR")
    if not xdg_runtime_dir:
        uid = os.getuid()
        fallback = f"/run/user/{uid}"
        if os.path.isdir(fallback):
            xdg_runtime_dir = fallback
        else:
            raise RuntimeError("XDG_RUNTIME_DIR is not set")
    return os.path.join(xdg_runtime_dir, "stasis", "stasis.sock")


def send_stasis_browser_activity() -> None:
    sock_path = stasis_socket_path()
    log_line(f"send socket={sock_path}")
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.settimeout(2.0)
        sock.connect(sock_path)
        sock.sendall(b"browser-activity")
        sock.shutdown(socket.SHUT_WR)
        # Drain response to complete request/response cycle.
        _ = sock.recv(4096)


def send_stasis_browser_inactive() -> None:
    sock_path = stasis_socket_path()
    log_line(f"send socket={sock_path}")
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.settimeout(2.0)
        sock.connect(sock_path)
        sock.sendall(b"browser-inactive")
        sock.shutdown(socket.SHUT_WR)
        _ = sock.recv(4096)


def read_message() -> Optional[Dict[str, Any]]:
    raw_len = sys.stdin.buffer.read(4)
    if len(raw_len) == 0:
      return None
    if len(raw_len) < 4:
      return None

    msg_len = struct.unpack("<I", raw_len)[0]
    payload = sys.stdin.buffer.read(msg_len)
    if len(payload) < msg_len:
      return None

    try:
      return json.loads(payload.decode("utf-8"))
    except Exception:
      return {"type": "invalid"}


def write_message(obj: Dict[str, Any]) -> None:
    data = json.dumps(obj, separators=(",", ":")).encode("utf-8")
    sys.stdout.buffer.write(struct.pack("<I", len(data)))
    sys.stdout.buffer.write(data)
    sys.stdout.buffer.flush()


def handle_browser_activity() -> Dict[str, Any]:
    try:
      send_stasis_browser_activity()
      log_line("ok browser-activity")
    except Exception as err:
      log_line(f"error {err!r}")
      return {"ok": False, "error": str(err)}

    return {"ok": True}


def handle_browser_inactive() -> Dict[str, Any]:
    try:
      send_stasis_browser_inactive()
      log_line("ok browser-inactive")
    except Exception as err:
      log_line(f"error {err!r}")
      return {"ok": False, "error": str(err)}

    return {"ok": True}


def main() -> int:
    while True:
      msg = read_message()
      if msg is None:
        return 0

      if msg.get("type") == "browser-activity":
        log_line("recv browser-activity")
        write_message(handle_browser_activity())
      elif msg.get("type") == "browser-inactive":
        log_line("recv browser-inactive")
        write_message(handle_browser_inactive())
      else:
        log_line(f"recv unsupported type={msg.get('type')!r}")
        write_message({"ok": False, "error": "unsupported message type"})


if __name__ == "__main__":
    raise SystemExit(main())
