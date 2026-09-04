# Part 6 - The Finale

This is the last part of the [Smart Greenhouse](../02_smart-greenhouse.md) tutorial. You play with the running system, tear it down - and then bring the whole application back with a single command, learning how `app_specs.yml` binds five cells into one deployable unit.

---

## Step 8 - Play with the Swarm

Everything is event-driven, so you can perturb the system from the CLI and watch the cells react - in Terminal 3 and in the browser at the same time.

Point Terminal 3 at the interesting events:

```bash
myrmic subscribe bed_state,watering_started,watering_stopped
```

**Let it rain.** Publish a `rain` event, like in Part 1:

```bash
myrmic publish rain 20
```

The sensor adds 20 percentage points on the next tick. If the pump was running, the agent notices the jump past the high target and shuts it off - watch `watering_stopped` arrive in Terminal 3 and the pump indicator go off in the browser.

**Replant the bed.** Tell the grow-bed its new plants want wetter soil:

```bash
myrmic send grow-bed set_target '{"low": 70, "high": 85}'
```

The grow-bed stores the new range and announces it on `bed_state`, and both subscribers react at once: the dashboard shows the new targets, and the agent - seeing the bed below its new low - starts the pump within a tick or two. One command, four cells involved, all visible in the browser.

Curious what else is flying around? Subscribe to everything:

```bash
myrmic subscribe
```

---

## Step 9 - Tear It All Down

Before the finale, a look behind the curtain. Every deployed cell came from a *class* - the built Wasm binary, content-addressed by hash:

```bash
myrmic cells classes list
```

Now dismantle the greenhouse, cell by cell. `teardown` removes a cell and, with `--remove-class`, its class:

```bash
myrmic cells teardown irrigation-agent --remove-class
myrmic cells teardown dashboard --remove-class
myrmic cells teardown grow-bed --remove-class
myrmic cells teardown pump --remove-class
myrmic cells teardown moisture-sensor --remove-class
```

Note the order: the agent goes first. It is the only cell that *acts*, so removing it first leaves nothing sending surprise commands while the rest is dismantled.

Verify the swarm is empty:

```bash
myrmic cells
```

```text
No cells registered
```

Leave the runtime and the gateway running - we are not done.

---

## Step 10 - One File, One Command

You built the greenhouse the developer's way: five times `myrmic new`, five deploys, one cell at a time, watching each piece come alive. But that is not how an application ships. An application is one unit - and Myrmic describes it in one file: the **application specification**.

Create `app_specs.yml` in the `greenhouse/` directory, next to the five crates:

```yaml
name: greenhouse

classes:
  - id: moisture-sensor
    build: ./moisture-sensor
  - id: pump
    build: ./pump
  - id: grow-bed
    build: ./grow-bed
  - id: dashboard
    build: ./dashboard
  - id: irrigation-agent
    build: ./irrigation-agent

instances:
  - class: moisture-sensor
  - class: pump
  - class: grow-bed
  - class: dashboard
  - class: irrigation-agent
```

`classes` says what to build, `instances` says what to run. An instance's SRN defaults to its class id - which is why the agent still finds the pump under the name `pump`. The same file can also pin platforms, pass init arguments, place cells on specific runtimes via tags, or run several instances of one class - see [Cell and application configuration](../../10_reference/01_configuration/02_cell-and-application-configuration.md).

Now resurrect the entire greenhouse with one command:

```bash
myrmic deploy app_specs.yml
```

The CLI builds all five cells and deploys them as the application `greenhouse`. Look at the swarm:

```bash
myrmic cells
```

```text
  cell              sri       kind  runtime     age  class             srn
──── greenhouse ────────────────────────────────────────────────────────────────────
  dashboard         884f...   wasm  [8]30d3436  20s  dashboard         dashboard
  grow-bed          fd02...   wasm  [8]30d3436  20s  grow-bed          grow-bed
  irrigation-agent  9f21...   wasm  [8]30d3436  20s  irrigation-agent  irrigation-agent
  moisture-sensor   365d...   wasm  [8]30d3436  20s  moisture-sensor   moisture-sensor
  pump              4e9b...   wasm  [8]30d3436  20s  pump              pump
```

The five cells are grouped under the application's name now. Refresh the browser: the dashboard is back, the readings tick, and within a couple of minutes the agent waters the bed - the whole machine, from one file.

And because the swarm knows the five belong together, they also leave together:

```bash
myrmic delete greenhouse --app
```

```text
INFO  deleted application 'greenhouse'
```

That is the full circle. Stop the gateway (Ctrl-C in Terminal 4) and the runtime:

```bash
myrmic runtimes stop
```

---

## What Have You Built

Five cells, four patterns, one application:

- A **mock sensor** (adapter) that publishes events on the runtime's scheduler.
- A **pump** (adapter) that exposes commands and announces its state.
- A **grow-bed** (asset) that owns the canonical state and shares it as structured events.
- An **irrigation agent** that holds all the business logic and commands the pump.
- A **dashboard** (adapter, gateway) that puts the bed in the browser.

Along the way you used the CLI as a full participant in the swarm - publishing, subscribing, sending commands, reading logs - and finished by shipping the whole thing as one unit. Every piece was deployed, evolved, and redeployed while the rest kept running: that is the everyday feel of developing on a swarm.

## Where to Go Next

- **Watch the swarm's internals** - traces of every command hop, including the agent's decisions, in the [Observability tutorial](../06_observability.md).
- **A richer gateway frontend** - the dashboard here polls a static file; the gateway also streams events to the browser and accepts commands from it. See the [gateway reference](../../10_reference/02_myrmic-cli/10_gateway.md) and the `chatty` example in the repository.
- **Revisit the patterns** - the [cell patterns](../../06_concepts/07_cell-patterns.md) page names what you just practiced: assets own state, adapters own actuation, agents own decisions.
