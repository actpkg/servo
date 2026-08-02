---
name: servo
description: Render a web page with the Servo engine, read its DOM, and interact with it — inside a sandbox, on the CPU, with no browser installed.
metadata:
  act: {}
---

# servo

A real browser engine as a component. It loads a page, runs its scripts, lays it
out, rasterizes it on the CPU and hands back a PNG and the DOM — with no browser,
no display server and no GPU on the host, and with network access gated by a
capability the caller has to grant.

## Two ways to use it

**One shot.** `render` takes `html` or `url` and returns two content parts: an
`image/png` screenshot and the `text/html` DOM as it stands after scripts have
run. Use this when you want to look at a page.

**A session.** Open one and the page stays live, so you can work with it:

| tool | what it does |
| --- | --- |
| `dom` | the page's HTML, after scripting |
| `eval` | run JavaScript, get the result as text |
| `click` | click a point, in whole CSS pixels from the top-left |
| `scroll` | scroll by a delta in whole CSS pixels |
| `screenshot` | capture the current state as a PNG |

`eval` is the general-purpose tool: it reaches anything the others do not. To
click a named element rather than a coordinate, ask the page where it is first:

```js
var r = document.querySelector('#submit').getBoundingClientRect();
JSON.stringify([Math.round(r.left + r.width / 2), Math.round(r.top + r.height / 2)])
```

then pass those numbers to `click`. Filling a field, submitting a form or waiting
for a selector are all `eval` too.

## One document per instance

**An instance renders exactly one document.** Opening a second session, or
navigating to a different page, will be refused — Servo gives every document a
script thread, every script thread creates a JavaScript context, and the engine
allows one of those per thread. There is one thread inside a component.

So: one page per instance. Start another instance for another page. Navigation
*within* a document — `history.pushState`, a fragment — works normally through
`eval`.

## Capabilities

- **`wasi:filesystem`** is required even for offline pages: the engine keeps
  client storage on disk and will not start without a writable directory. Scope
  the grant to a temporary directory.
- **`wasi:sockets`** is required to load anything over the network. Without it,
  `http:` and `https:` fail and the page renders the engine's own error document —
  inline HTML still works. Granting it lets the page reach whatever it points at,
  so scope the grant if the page is not trusted.

## What it does not do

No WebGL or GPU rendering — everything is rasterized on the CPU, so a heavy page
is slow rather than impossible. No colour emoji (the build drops PNG-compressed
bitmap strikes). Only the fonts compiled into the component are available:
DejaVu Sans, Serif and Sans Mono, which back the CSS generic families.

## Settling

A page's load event fires long before it stops changing. `render` and
`screenshot` take `settle_ms` — the time to keep driving the page before
capturing. The default of 300ms suits static pages; raise it for deferred
scripts, WebSocket traffic or late images.
