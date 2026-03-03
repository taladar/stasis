(() => {
  if (globalThis.__stasisBrowserActivityInstalled) {
    return;
  }
  globalThis.__stasisBrowserActivityInstalled = true;

  const KEEPALIVE_MIN_INTERVAL_MS = 20000;
  const MEDIA_SCAN_INTERVAL_MS = 2000;
  const INACTIVE_DEBOUNCE_MS = 1000;
  const MEDIA_STALL_GRACE_MS = 8000;
  const MEDIA_TIME_EPSILON_S = 0.05;

  let lastKeepaliveSentMs = 0;
  let lastMediaActive = false;
  let inactiveTimer = null;
  const wiredMediaEls = new WeakSet();
  const mediaProgress = new WeakMap();

  function mediaElementLooksActive(el, nowMs) {
    if (!el) {
      return false;
    }

    if (typeof el.paused === "boolean" && el.paused) {
      return false;
    }
    if (typeof el.ended === "boolean" && el.ended) {
      return false;
    }
    if (typeof el.playbackRate === "number" && el.playbackRate <= 0) {
      return false;
    }

    const current = typeof el.currentTime === "number" ? el.currentTime : 0;
    const prev = mediaProgress.get(el);
    if (!prev) {
      mediaProgress.set(el, { t: current, lastProgressMs: nowMs });
      return true;
    }

    if (Math.abs(current - prev.t) >= MEDIA_TIME_EPSILON_S) {
      mediaProgress.set(el, { t: current, lastProgressMs: nowMs });
      return true;
    }

    mediaProgress.set(el, { t: current, lastProgressMs: prev.lastProgressMs });
    return nowMs - prev.lastProgressMs <= MEDIA_STALL_GRACE_MS;
  }

  function hasActiveMediaOnPage() {
    const now = Date.now();
    const mediaEls = document.querySelectorAll("video, audio");
    for (const el of mediaEls) {
      if (mediaElementLooksActive(el, now)) {
        return true;
      }
    }
    return false;
  }

  function sendKeepalive(reason, force = false) {
    if (document.visibilityState !== "visible") {
      return;
    }

    const now = Date.now();
    if (!force && now - lastKeepaliveSentMs < KEEPALIVE_MIN_INTERVAL_MS) {
      return;
    }

    lastKeepaliveSentMs = now;

    try {
      chrome.runtime.sendMessage({
        type: "stasis.browser_keepalive",
        reason,
        ts: now,
        url: location.href,
      });
    } catch (_) {
      // Extension context may not be available during unload/startup races.
    }
  }

  function sendInactive(reason) {
    if (inactiveTimer !== null) {
      clearTimeout(inactiveTimer);
      inactiveTimer = null;
    }

    try {
      chrome.runtime.sendMessage({
        type: "stasis.browser_inactive",
        reason,
        ts: Date.now(),
        url: location.href,
      });
    } catch (_) {
      // Extension context may not be available during unload/startup races.
    }
  }

  function sendInactiveDebounced(reason) {
    if (inactiveTimer !== null) {
      clearTimeout(inactiveTimer);
      inactiveTimer = null;
    }

    inactiveTimer = setTimeout(() => {
      inactiveTimer = null;
      sendInactive(reason);
    }, INACTIVE_DEBOUNCE_MS);
  }

  function checkMediaAndSignal(reason = "media-scan") {
    const active = hasActiveMediaOnPage();

    if (active) {
      if (inactiveTimer !== null) {
        clearTimeout(inactiveTimer);
        inactiveTimer = null;
      }
      // Transition to active should pulse immediately so first-play is caught.
      sendKeepalive(reason, !lastMediaActive);
    } else if (lastMediaActive) {
      // Explicit falling edge: media just stopped.
      sendInactiveDebounced("media-stopped");
    }

    lastMediaActive = active;
  }

  function wireMediaElement(el) {
    if (!el || wiredMediaEls.has(el)) {
      return;
    }
    wiredMediaEls.add(el);

    const onPlayLike = () => checkMediaAndSignal("media-event-play");
    const onStopLike = () => checkMediaAndSignal("media-event-stop");
    const onTimeUpdate = () => checkMediaAndSignal("media-event-timeupdate");

    el.addEventListener("play", onPlayLike, { passive: true });
    el.addEventListener("playing", onPlayLike, { passive: true });
    el.addEventListener("pause", onStopLike, { passive: true });
    el.addEventListener("ended", onStopLike, { passive: true });
    el.addEventListener("timeupdate", onTimeUpdate, { passive: true });
  }

  function wireExistingMediaElements() {
    for (const el of document.querySelectorAll("video, audio")) {
      wireMediaElement(el);
    }
  }

  // Catch already-playing media when script attaches to an existing tab.
  wireExistingMediaElements();
  checkMediaAndSignal("startup");
  window.setInterval(checkMediaAndSignal, MEDIA_SCAN_INTERVAL_MS);

  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState !== "visible") {
      sendInactive("hidden");
    } else {
      checkMediaAndSignal("visible");
    }
  });

  const observer = new MutationObserver((_mutations) => {
    wireExistingMediaElements();
  });
  observer.observe(document.documentElement, {
    childList: true,
    subtree: true,
  });

  window.addEventListener("pagehide", () => sendInactive("pagehide"), {
    capture: true,
    passive: true,
  });
})();
