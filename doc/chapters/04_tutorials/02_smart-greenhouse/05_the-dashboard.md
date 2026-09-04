# Part 5 - The Dashboard

This is Part 5 of the [Smart Greenhouse](../02_smart-greenhouse.md) tutorial. You put the bed in the browser - learning how a cell serves a web page through the gateway.

It continues where [Part 4](./04_the-irrigation-agent.md) left off: all four cells are deployed and the greenhouse is watering itself.

---

## Step 7 - The Dashboard

Everything the greenhouse knows still lives inside the swarm - and browsers do not speak what the swarm speaks, they speak HTTP. The bridge between the two worlds is the myrmic **gateway**: an HTTP server that cells register their routes on. To put the bed on a web page we build one more cell, following the same pattern as the pump: an adapter. The pump adapts a motor; the dashboard, along with the gateway, adapts the browser.

One thing to be clear about up front: the dashboard cell does not *run* the gateway. The gateway is its own process (it gets Terminal 4 in a moment); the cell registers its routes on it and fills the store the gateway serves from. Tear the cell down and its routes and files disappear with it - the gateway keeps running.

Scaffold the cell and add the `serde` line to `dashboard/Cargo.toml`, as in Part 3:

```bash
myrmic new dashboard
```

```toml
serde = { version = "1", default-features = false, features = ["alloc", "derive"] }
```

Replace the content of `dashboard/src/lib.rs` with:

```rust
//! Dashboard adapter: serves the greenhouse web page through the gateway.
//! It mirrors the grow-bed's `bed_state` into a small JSON snapshot that the
//! page polls. It owns no truth and makes no decisions.
#![no_std]

use myrmic_sdk::{Metadata, Result, format, gateway};

/// Payload of the grow-bed's `bed_state` event.
#[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
struct BedState {
    moisture: f32,
    pump_on: bool,
    target_low: f32,
    target_high: f32,
}

#[myrmic_sdk::init]
fn init(md: Metadata) -> Result<()> {
    gateway::assets(md.id).put("/index.html", include_bytes!("../assets/index.html"))?;
    gateway::mount("/greenhouse")
        .index("/index.html")
        .bind()
        .map_err(<&'static str>::from)?;
    Ok(())
}

/// Every `bed_state` update becomes a fresh `latest.json` for the page to poll.
#[myrmic_sdk::evt]
fn bed_state(md: Metadata, bed: BedState) -> Result<()> {
    let json = format!(
        r#"{{"moisture":{:.1},"pump_on":{},"target_low":{},"target_high":{}}}"#,
        bed.moisture, bed.pump_on, bed.target_low, bed.target_high
    );
    gateway::assets(md.id).put("/latest.json", json.as_bytes())
}
```

Reading it top to bottom:

- The cell declares its *own* `BedState` struct matching the event's JSON fields - cells share wire formats, not Rust types. Each side owns its definition.
- `gateway::assets(md.id)` - the cell's private blob store for static files. Anything written there is served by the gateway once the cell mounts a route. At init the cell uploads the web page into it.
- `gateway::mount("/greenhouse")` - claims the URL prefix on the gateway. `.index("/index.html")` serves that file at the mount root and enables static serving for everything else in the store. See the [gateway reference](../../10_reference/02_myrmic-cli/10_gateway.md) for the full mount API (APIs, WebSockets, fallbacks).
- The `bed_state` handler is the whole application logic of this cell: every announcement from the asset is rewritten as `latest.json` in the store. The file is a *read model* - a derived copy optimized for display. The truth stays in the grow-bed; delete this cell and no information is lost.

Now create the web page at `dashboard/assets/index.html`:

```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Smart Greenhouse</title>
  <style>
    body { font-family: sans-serif; max-width: 28rem; margin: 3rem auto; }
    h1 { font-size: 1.4rem; }
    .value { font-size: 3rem; font-variant-numeric: tabular-nums; }
    meter { width: 100%; height: 1.5rem; }
    .row { margin: 0.75rem 0; }
    #pump.on { color: #1a7f37; font-weight: bold; }
  </style>
</head>
<body>
  <h1>Smart Greenhouse - Grow Bed 1</h1>
  <div class="row">Soil moisture: <span class="value" id="moisture">--</span>%</div>
  <div class="row"><meter id="bar" min="0" max="100" value="0"></meter></div>
  <div class="row">Pump: <span id="pump">--</span></div>
  <div class="row">Target range: <span id="target">--</span></div>

  <script>
    async function refresh() {
      const res = await fetch('/greenhouse/latest.json', { cache: 'no-store' });
      if (!res.ok) return;
      const data = await res.json();
      document.getElementById('moisture').textContent = data.moisture.toFixed(1);
      document.getElementById('bar').value = data.moisture;
      const pump = document.getElementById('pump');
      pump.textContent = data.pump_on ? 'RUNNING' : 'off';
      pump.className = data.pump_on ? 'on' : '';
      document.getElementById('target').textContent =
        data.target_low + '% - ' + data.target_high + '%';
    }

    async function main() {
      // Poll the gateway forever: one HTTP call every 2 seconds.
      for (;;) {
        try { await refresh(); } catch (e) { /* gateway restarting */ }
        await new Promise(resolve => setTimeout(resolve, 2000));
      }
    }
    main();
  </script>
</body>
</html>
```

The page is deliberately plain: a loop that fetches `latest.json` every 2 seconds and writes the values into the DOM. No framework, no build step.

One thing to know before deploying: `include_bytes!` bakes the page into the Wasm binary, and a cell's memory is a fixed budget declared in its `Cargo.toml`. Our small page still fits the default budget - but if yours grows (more markup, some CSS, an embedded image), the build stops with:

```text
rust-lld: error: initial memory too small, <N> bytes needed
```

If that happens, give the cell more room in `dashboard/Cargo.toml`:

```toml
[package.metadata.myrmic]
heap_size = 65_536
initial_memory = 262_144
max_memory = 262_144
```

See [Cell and application configuration](../../10_reference/01_configuration/02_cell-and-application-configuration.md) for what these values mean. Now deploy:

```bash
myrmic deploy dashboard
```

Time to open the doors. The gateway is the swarm's HTTP entry point - start it in Terminal 4 and leave it running:

```bash
myrmic gateway
```

The default port is 8080 (`--port` changes it). Open your browser at:

```text
http://localhost:8080/greenhouse/
```

There is the bed: the moisture value updating as the mock's weather swings, the target range from the asset, the pump state. And the page is not just showing the weather - watch long enough and you will see Part 4's agent at work: the pump indicator flipping to RUNNING at the low target and off again at the high one, with nobody at the keyboard.

Follow the data on its round trip: sensor → `moisture` event → grow-bed → `bed_state` event → dashboard → `latest.json` in its store → gateway → HTTP → browser. No cell ever runs a web server; the dashboard just writes files and the gateway serves them.

---

## What Have You Learned

- The gateway is the swarm's HTTP entry point. Cells do not run it - they register routes on it, and the routes and files live and die with the cell.
- `gateway::assets(...)` is a per-cell blob store; whatever a cell writes there, the gateway serves over HTTP once the cell mounts a route with `.index(...)` or `.assets()`.
- A dashboard keeps a *read model*: a derived copy of the asset's state, optimized for display. The truth stays in the asset - deleting the dashboard loses nothing.
- `include_bytes!` bakes files into the Wasm binary - and larger binaries need more memory, configured in `[package.metadata.myrmic]`.

## Next Step

The greenhouse is complete: measured, remembered, decided, displayed. In [Part 6 - The Finale](./06_the-finale.md) you play with the running system, tear it down, and bring it all back with a single command.
