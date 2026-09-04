# Smart Greenhouse

In this tutorial you build a complete Myrmic application on a single computer: a smart greenhouse that keeps the soil of a grow bed inside a target moisture range - and a live web dashboard to watch it work.

You will write five cells, wire them together with events and commands, and drive the whole thing from the Myrmic CLI. Along the way you will use most of the CLI's day-to-day functionality: scaffolding, building, deploying, listing cells, sending commands, publishing and subscribing to events, serving a web frontend through the gateway, reading logs, and tearing everything down again.

No hardware is required - the sensor is mocked in software.

## What You Will Build

Five cells, one per role:

| Cell | Pattern | Responsibility |
|---|---|---|
| `moisture-sensor` | Adapter (mocked) | Publishes a `moisture` reading every second. The mock value swings between dry and wet, standing in for real weather and soil physics. |
| `pump` | Adapter (actuator) | Accepts `start` / `stop` commands and announces its state on the `pump_state` event. Knows nothing about plants or policy. |
| `grow-bed` | Asset | Owns the canonical state of one bed of plants: latest moisture, pump status, and the moisture range the plants want. Announces every change on the `bed_state` event. |
| `dashboard` | Adapter (gateway) | Mirrors `bed_state` into a JSON snapshot and serves the web page at `/greenhouse` through the gateway. Owns no truth, makes no decisions. |
| `irrigation-agent` | Agent | The only cell that makes decisions: reads the bed's `bed_state` and drives the pump with hysteresis. |

The roles follow the [cell patterns](../06_concepts/07_cell-patterns.md): **assets own state, adapters own actuation, agents own decisions**. The layering is strict: the raw `moisture` event is a detail of the sensor adapter, consumed only by the asset; everyone else - the agent included - reads the grow-bed's canonical `bed_state`. Nothing commands the pump except the agent, and the grow-bed commands nothing at all - if you later reuse it in an application without irrigation, it carries no dead weight. And the sensor and the pump do not know each other at all: in the real world they are connected by soil and water, not by software.

```text
                  moisture (event)            bed_state (event)
  moisture-sensor ──────────────► grow-bed ────────┬────────► irrigation-agent
                                (canonical state)  │                 │
                                      ▲            ▼                 │
                    pump_state (event)│        dashboard             │ start / stop
                                      │            │ latest.json     │ (commands)
                                      │            ▼                 │
                                      │     myrmic gateway ◄─ browser│
                                     pump ◄──────────────────────────┘
```

To facilitate the visualization and understanding, we will use several terminals:

| Terminal | Runs |
|---|---|
| Terminal 1 | the Myrmic runtime |
| Terminal 2 | your working shell: scaffold, build, deploy, send |
| Terminal 3 | `myrmic subscribe` - a live view of the events flying around |
| Terminal 4 | the gateway |

## Prerequisites

- The Myrmic CLI installed and the Rust toolchain set up - complete the [Quickstart](../01_quickstart.md) first if you have not.
- A web browser.

## Tutorial Parts

1. [The Mock Sensor](./02_smart-greenhouse/01_the-mock-sensor.md) - start the runtime, scaffold your first cell, and build a mock sensor - **learning how cells publish and subscribe to events.**
2. [The Pump](./02_smart-greenhouse/02_the-pump.md) - build the actuator that waters the bed - **learning how to use cell commands.**
3. [The Grow-Bed](./02_smart-greenhouse/03_the-grow-bed.md) - build the Asset Cell that owns the bed's canonical state - **learning how to model and share structured state.**
4. [The Irrigation Agent](./02_smart-greenhouse/04_the-irrigation-agent.md) - automate the watering decision - **learning how cells send commands to each other.**
5. [The Dashboard](./02_smart-greenhouse/05_the-dashboard.md) - put the bed on a web page served through the gateway - **learning how cells face the browser.**
6. [The Finale](./02_smart-greenhouse/06_the-finale.md) - play with the swarm, tear it down, and bring it all back with one command - **learning how `app_specs.yml` ships an application as one unit.**
