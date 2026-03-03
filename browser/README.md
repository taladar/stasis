# Browser Activity Bridge

This folder is reserved for browser integration scripts.

Goal:
- Browser activity should keep Stasis in a waiting-for-idle state.
- Browser activity should NOT increment inhibitor counters.

Current wire command:

```bash
stasis browser-activity
```

Use this from a native-host bridge or helper process when real user activity is
observed in a browser tab (e.g. key/mouse/scroll events).
