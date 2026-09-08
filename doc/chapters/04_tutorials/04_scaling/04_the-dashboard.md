# Part 4 - The Dashboard

This is the last part of the [Scaling Your Application](../04_scaling.md) tutorial. You put the greenhouse on a web page again - and this time the page has two inputs: one that adds a bed, one that adds a sensor, each in the sector you type - **learning how a browser sends commands into the swarm through the gateway.**

It continues where [Part 3](./03_one-sensor-per-bed.md) left off: four beds, four sensors, both roots running.

---

## Step 13 - A Dashboard That Talks Back

The Smart Greenhouse dashboard only *showed* the swarm: the cell mirrored an event into a JSON file, the gateway served the file, the page polled it. That half stays. The new half is the reverse direction: the page sends `add_bed` and `add_sensor` to the roots, through the same gateway.

```bash
myrmic new dashboard
```

Replace the content of `dashboard/src/lib.rs` with:

```rust
//! Dashboard adapter: serves the greenhouse page and relays its buttons to the roots.
#![no_std]

use myrmic_sdk::db::state::State;
use myrmic_sdk::{Metadata, Result, Vec, gateway};

/// Payload of a grow-bed's `bed_state` event. Declared here as well as in the
/// grow-bed: cells share wire formats, not Rust types.
#[derive(Clone, serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
struct BedState {
    index: u32,
    moisture: f32,
}

/// The latest state of every bed heard from, by index.
const BEDS: State<Vec<BedState>> = State::new_const("beds");

#[myrmic_sdk::init]
fn init(md: Metadata) -> Result<()> {
    gateway::assets(md.id).put("/index.html", include_bytes!("../assets/index.html"))?;
    gateway::mount("/greenhouse")
        .index("/index.html")
        // GET opens a session, POST sends a command: the page's two buttons.
        .api("/api")
        .bind()
        .map_err(<&'static str>::from)?;

    Ok(())
}

/// Every `bed_state` refreshes that bed's entry and rewrites `beds.json`.
#[myrmic_sdk::evt]
fn bed_state(md: Metadata, bed: BedState) -> Result<()> {
    let mut beds = BEDS.load()?.unwrap_or_default();
    match beds.iter_mut().find(|b| b.index == bed.index) {
        Some(slot) => *slot = bed,
        None => {
            beds.push(bed);
            beds.sort_by_key(|b| b.index);
        }
    }
    BEDS.save(&beds)?;

    let json = serde_json::to_vec(&beds).map_err(|_| "json")?;
    gateway::assets(md.id).put("/beds.json", &json)
}
```

Two things differ from the Smart Greenhouse dashboard. The read model is a list, one entry per bed, because there are many beds now; every `bed_state` replaces its bed's entry and rewrites `beds.json`. And the mount gains `.api("/api")`: a path on which the gateway accepts messages from the browser and drops them into cells' mailboxes. The cell does nothing for that path - it only declares it.

Add `serde_json` to `dashboard/Cargo.toml`, and give the cell the memory a baked-in page needs:

```toml
[package.metadata.myrmic]
heap_size = 65_536
initial_memory = 262_144
max_memory = 262_144

[dependencies]
myrmic-sdk = ...            # as generated
serde = { version = "1", default-features = false, features = ["alloc", "derive"] }
serde_json = { version = "1", default-features = false, features = ["alloc"] }
```

Now the page. Create `dashboard/assets/index.html` with:

```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Smart Greenhouse</title>
  <style>
    body { font-family: sans-serif; max-width: 40rem; margin: 3rem auto; }
    h1 { font-size: 1.4rem; }
    .beds { display: grid; grid-template-columns: repeat(auto-fill, minmax(10rem, 1fr)); gap: 1rem; }
    .bed { border: 1px solid #ccc; border-radius: 6px; padding: 0.75rem; }
    .bed .value { font-size: 2rem; font-variant-numeric: tabular-nums; }
    .bed meter { width: 100%; height: 1rem; }
    .bed .none { color: #888; font-style: italic; }
    form { display: flex; gap: 0.5rem; align-items: center; margin: 1rem 0; }
    #status { color: #888; font-size: 0.9rem; }
  </style>
</head>
<body>
  <h1>Smart Greenhouse</h1>
  <p id="status">connecting to the gateway...</p>

  <div class="beds" id="beds"></div>

  <form id="add-bed">
    <label>Sector <input id="bed-sector" value="sectorA" required></label>
    <button type="submit">Add bed</button>
  </form>
  <form id="add-sensor">
    <label>Sector <input id="sensor-sector" value="sectorA" required></label>
    <button type="submit">Add sensor</button>
  </form>

  <script>
    // ---- reading: beds.json, kept up to date by the dashboard cell ----
    async function refresh() {
      const res = await fetch('/greenhouse/beds.json', { cache: 'no-store' });
      if (!res.ok) return;
      const beds = await res.json();
      document.getElementById('beds').innerHTML = beds.map(b => `
        <div class="bed">
          <strong>Bed ${b.index}</strong><br>
          ${b.moisture > 0
            ? `<span class="value">${b.moisture.toFixed(1)}</span>%<br><meter min="0" max="100" value="${b.moisture}"></meter>`
            : `<span class="none">no sensor yet</span>`}
        </div>`).join('');
    }

    // ---- writing: commands to the roots through the gateway's API path ----
    // GET /greenhouse/api opens a session; its first message carries the
    // session id, which every POST must present.
    let session = null;
    const stream = new EventSource('/greenhouse/api');
    stream.onmessage = (e) => {
      const msg = JSON.parse(e.data);
      if (msg.type === 'ready') {
        session = msg.session;
        document.getElementById('status').textContent = 'connected';
      }
    };
    stream.onerror = () => { document.getElementById('status').textContent = 'gateway unreachable'; };

    async function command(target, name, payload) {
      if (!session) return;
      const res = await fetch('/greenhouse/api', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'X-Gateway-Session': session },
        // The payload is JSON, carried as base64 inside the envelope.
        body: JSON.stringify({ type: 'command', sri: target, name, payload: btoa(JSON.stringify(payload)) }),
      });
      document.getElementById('status').textContent =
        res.ok ? `sent ${name} to ${target}` : `${name} failed: ${await res.text()}`;
    }

    document.getElementById('add-bed').onsubmit = (e) => {
      e.preventDefault();
      command('bed-root', 'add_bed', document.getElementById('bed-sector').value.trim());
    };
    document.getElementById('add-sensor').onsubmit = (e) => {
      e.preventDefault();
      command('sensor-root', 'add_sensor', document.getElementById('sensor-sector').value.trim());
    };

    async function main() {
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

The reading half is the Smart Greenhouse loop, polling `beds.json` every two seconds. The writing half is new, and worth following once:

1. `GET /greenhouse/api` opens a stream. Its first message is `{"type":"ready","session":"..."}`: the gateway has created a **session** for this browser tab. While the stream is open, the session is alive.
2. A command is a `POST` to the same path, with the session id in the `X-Gateway-Session` header and a small JSON envelope: the target cell as an SRN, the command name, and the payload as base64. The payload itself is what `myrmic send` would take - here the JSON string `"sectorA"`.
3. The gateway resolves the SRN, checks that the cell exists, and puts the command in its mailbox. The response is `202 Accepted`; the command runs in the swarm, and the page learns the outcome the same way anyone does - by watching the swarm change.

The page names its targets `bed-root` and `sensor-root`. It can, because they are roots with fixed names from `app_specs.yml`; it never handles a UUID.

---

## Step 14 - Grow the Greenhouse From the Browser

Add the dashboard to `app_specs.yml` - a class and a root instance:

```yaml
  - id: dashboard
    build: ./dashboard
```

```yaml
  - class: dashboard
    restart: always
```

Remove and deploy, and start the gateway in Terminal 3 and leave it running:

```bash
myrmic delete greenhouse --app
myrmic deploy app_specs.yml
```

```bash
myrmic gateway
```

```bash
myrmic cells
```

```text
  cell         sri                                   kind  runtime     age  policy  class            srn
──── greenhouse ────────────────────────────────────────────────────────────────────────────────────────────────────────
  bed-root     ec2709f8-5e11-5ca8-a152-5d195d73a00a  wasm  [1]12a7f3f  11s  always  bed-root         bed-root
  ├─ bed-1     777e343f-cc44-5964-850d-9302dbd591ee  wasm  [1]12a7f3f  10s  —       grow-bed         bed-root/bed-1
  ├─ bed-2     f1356c6d-8603-5fa5-a659-35764d15a3bf  wasm  [d]40df21c  9s   —       grow-bed         bed-root/bed-2
  ├─ bed-3     72fdd5d5-ca6b-54af-b017-c624f8b8b1d7  wasm  [c]a157d28  9s   —       grow-bed         bed-root/bed-3
  └─ bed-4     22bffd66-a421-5ddb-8f3e-6530c8fd010a  wasm  [4]f741c9b  9s   —       grow-bed         bed-root/bed-4
  dashboard    884f63a0-83ed-52fe-b67c-2bcfb6df378c  wasm  [c]a157d28  11s  always  dashboard        dashboard
  sensor-root  904b6b3b-8411-5ff5-8f0d-56555e49b75e  wasm  [4]f741c9b  11s  always  sensor-root      sensor-root
  ├─ sensor-1  152b9c54-519c-55e4-b828-ecc811f5325c  wasm  [1]12a7f3f  10s  —       moisture-sensor  sensor-root/sensor-1
  ├─ sensor-2  880e731a-aa75-5bab-9377-73b271c5643a  wasm  [d]40df21c  10s  —       moisture-sensor  sensor-root/sensor-2
  ├─ sensor-3  025bda92-5332-557a-a2a7-ef8d8933c4a5  wasm  [c]a157d28  9s   —       moisture-sensor  sensor-root/sensor-3
  └─ sensor-4  b86e8291-adb5-55e3-b058-1352fc57b966  wasm  [4]f741c9b  9s   —       moisture-sensor  sensor-root/sensor-4
```

Both trees came back on their own, as they do by now, and the dashboard is a third root. Open the browser:

```text
http://localhost:8080/greenhouse/
```

Four bed cards with moving readings, and below them the two forms. Type `sectorB` into both, press **Add bed**, then **Add sensor**. A fifth card appears saying *no sensor yet*; a few seconds later it shows a reading. In Terminal 1:

```bash
myrmic cells
```

```text
  cell         sri                                   kind  runtime     age  policy  class            srn
──── greenhouse ────────────────────────────────────────────────────────────────────────────────────────────────────────
  bed-root     ec2709f8-5e11-5ca8-a152-5d195d73a00a  wasm  [1]12a7f3f  25s  always  bed-root         bed-root
  ├─ bed-1     777e343f-cc44-5964-850d-9302dbd591ee  wasm  [1]12a7f3f  24s  —       grow-bed         bed-root/bed-1
  ├─ bed-2     f1356c6d-8603-5fa5-a659-35764d15a3bf  wasm  [d]40df21c  24s  —       grow-bed         bed-root/bed-2
  ├─ bed-3     72fdd5d5-ca6b-54af-b017-c624f8b8b1d7  wasm  [c]a157d28  24s  —       grow-bed         bed-root/bed-3
  ├─ bed-4     22bffd66-a421-5ddb-8f3e-6530c8fd010a  wasm  [4]f741c9b  23s  —       grow-bed         bed-root/bed-4
  └─ bed-5     a8e580cb-81d5-595e-b715-452ca176e4b6  wasm  [d]40df21c  11s  —       grow-bed         bed-root/bed-5
  dashboard    884f63a0-83ed-52fe-b67c-2bcfb6df378c  wasm  [c]a157d28  25s  always  dashboard        dashboard
  sensor-root  904b6b3b-8411-5ff5-8f0d-56555e49b75e  wasm  [4]f741c9b  25s  always  sensor-root      sensor-root
  ├─ sensor-1  152b9c54-519c-55e4-b828-ecc811f5325c  wasm  [1]12a7f3f  24s  —       moisture-sensor  sensor-root/sensor-1
  ├─ sensor-2  880e731a-aa75-5bab-9377-73b271c5643a  wasm  [d]40df21c  24s  —       moisture-sensor  sensor-root/sensor-2
  ├─ sensor-3  025bda92-5332-557a-a2a7-ef8d8933c4a5  wasm  [c]a157d28  24s  —       moisture-sensor  sensor-root/sensor-3
  ├─ sensor-4  b86e8291-adb5-55e3-b058-1352fc57b966  wasm  [4]f741c9b  23s  —       moisture-sensor  sensor-root/sensor-4
  └─ sensor-5  3bc2a4a0-90f6-523c-b9d7-b2d93278c3d7  wasm  [4]f741c9b  7s   —       moisture-sensor  sensor-root/sensor-5
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
  —            2b2ae79a-589a-42de-966d-a0c0c6920890  N/A   —           14s  —       —                —
```

`bed-5` and `sensor-5`, both in sector B, both created from a browser. The button went browser → gateway → `bed-root`'s mailbox → `add_bed` → `spawn_bed`: the same path `myrmic send` took in Part 1, with a different client at the front. The nameless row below the rule is your browser tab: the gateway registers each open session as a placeholder cell so that cells can address replies to it, and the row disappears half a minute or so after the tab is closed. And the file the page polls now has five entries:

```text
[{"index":1,"moisture":55.399963},{"index":2,"moisture":54.99996},{"index":3,"moisture":55.399963},{"index":4,"moisture":55.399963},{"index":5,"moisture":62.19999}]
```

Everything you did in Parts 1 to 3 still holds for beds made this way. Kill the node under `bed-5` and the bed root brings it back; kill the node under the bed root and the list, now with five entries, rebuilds the tree.

---

## Step 15 - Clean Up

```bash
myrmic delete greenhouse --app
myrmic replicate app:greenhouse -e linux
myrmic telemetry no-db-retention
myrmic runtimes delete node1 node2 node3 node4
```

Stop the gateway with Ctrl-C in Terminal 3.

---

## What Have You Built

A greenhouse that grows while it runs:

- Two **roots that spawn** - one for beds, one for sensors - each remembering a short list and turning it into running cells: on request, after a loss, and after their own restart.
- **Children with fixed identities**, so that a respawn is a resume and a bed can find its sensor by name alone.
- **Placement from user input**: the sector typed into a command or a form decides where the new cell runs.
- A **dashboard that talks back**, sending commands to cells it names by SRN through the gateway.

The layout of the greenhouse - how many beds, how many sensors, which sector - is nowhere in `app_specs.yml`. It lives in the roots' state, replicated, and the swarm rebuilds it from there.

## What Have You Learned

- A page reaches the swarm through the gateway's **API path**: `GET` opens a session, `POST` sends an envelope with the target's SRN, the command name and a base64 payload.
- The gateway does for the browser what `myrmic send` does for the terminal: it drops a command into a mailbox. Nothing in the cells knows the difference.
- An open browser session is visible in `myrmic cells` as a placeholder row.
- Spawning, respawning and rebuilding do not care who asked. A bed added from a form is a bed like any other.

> **Preview version.** Everything in this tutorial is supervision done *by your code* with the primitives available today: fixed names, `on_cell_lost`, a list in state, `myrmic replicate`. A restart policy applies to roots only, and none of this is yet part of the [guarantee contract](../../08_guarantees.md). The [Roadmap](../../09_roadmap.md) describes how supervising a tree becomes something the swarm does by itself.

## Where to Go Next

- [Cell classes and spawning](../../10_reference/03_myrmic-sdk/04_cell-lifecycle/01_cell-classes-and-spawning.md) and [Cell monitoring](../../10_reference/03_myrmic-sdk/04_cell-lifecycle/03_cell-monitoring.md) - every spawn option, every loss reason.
- [Gateway routes](../../10_reference/03_myrmic-sdk/06_external-systems/01_gateway-routes.md) - the API and WebSocket paths.
- [`myrmic replicate`](../../10_reference/02_myrmic-cli/15_replicate.md) - replicating an application, a cell, or a scope.
- [Failure Behaviour](../../07_architecture/04_failure-behaviour.md) - what happens when a node is lost, in detail.
