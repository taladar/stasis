const NATIVE_HOST = "io.github.saltnpepper97.stasis";
const FORWARD_MIN_INTERVAL_MS = 8000;

let nativePort = null;
let connectAttempted = false;
let lastForwardMs = 0;

function ensureNativePort() {
  if (nativePort || connectAttempted) {
    return;
  }

  connectAttempted = true;
  try {
    nativePort = chrome.runtime.connectNative(NATIVE_HOST);

    nativePort.onDisconnect.addListener(() => {
      const err = chrome.runtime.lastError;
      if (err && err.message) {
        console.warn("stasis extension: native port disconnected:", err.message);
      }
      nativePort = null;
      connectAttempted = false;
    });

    nativePort.onMessage.addListener((_msg) => {
      // Keep channel healthy; host acknowledgements are optional for now.
    });
  } catch (err) {
    console.warn("stasis extension: failed to connect native host:", err);
    nativePort = null;
    connectAttempted = false;
  }
}

function forwardBrowserActivity(msg) {
  const now = Date.now();
  if (now - lastForwardMs < FORWARD_MIN_INTERVAL_MS) {
    return;
  }

  lastForwardMs = now;
  ensureNativePort();

  if (!nativePort) {
    return;
  }

  try {
    nativePort.postMessage({
      type: "browser-activity",
      reason: typeof msg.reason === "string" ? msg.reason : "activity",
      ts: typeof msg.ts === "number" ? msg.ts : now,
    });
  } catch (err) {
    console.warn("stasis extension: failed to send native message:", err);
    nativePort = null;
    connectAttempted = false;
  }
}

function forwardBrowserInactive() {
  ensureNativePort();
  if (!nativePort) {
    return;
  }

  try {
    nativePort.postMessage({
      type: "browser-inactive",
      ts: Date.now(),
    });
  } catch (err) {
    console.warn("stasis extension: failed to send inactive message:", err);
    nativePort = null;
    connectAttempted = false;
  }
}

chrome.runtime.onMessage.addListener((msg, _sender, _sendResponse) => {
  if (!msg) {
    return;
  }

  if (msg.type !== "stasis.browser_activity" && msg.type !== "stasis.browser_keepalive") {
    if (msg.type === "stasis.browser_inactive") {
      forwardBrowserInactive();
    }
    return;
  }

  forwardBrowserActivity(msg);
});

if (chrome.runtime.onStartup) {
  chrome.runtime.onStartup.addListener(() => {
    injectContentScriptsIntoExistingTabs();
  });
}

if (chrome.runtime.onInstalled) {
  chrome.runtime.onInstalled.addListener(() => {
    injectContentScriptsIntoExistingTabs();
  });
}

async function injectContentScriptsIntoExistingTabs() {
  // MV3 Chromium: ensure already-open tabs get media/activity probing.
  if (!chrome.scripting || !chrome.tabs?.query) {
    return;
  }

  try {
    const tabs = await chrome.tabs.query({
      url: ["http://*/*", "https://*/*"],
    });

    for (const tab of tabs) {
      if (!tab.id) {
        continue;
      }

      try {
        await chrome.scripting.executeScript({
          target: { tabId: tab.id },
          files: ["src/content.js"],
        });
      } catch (_err) {
        // Some tabs/URLs are not injectable (store pages, restricted schemes).
      }
    }
  } catch (_err) {
    // No-op; best-effort only.
  }
}

injectContentScriptsIntoExistingTabs();
