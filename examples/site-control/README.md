# Site Control

An edge application that keeps one site's temperature in range. **The cell that
touches the hardware runs on the device that has the hardware, not necessarily
on the machine running the policy.**

The controller cell reads a `temperature` tap and drives a `heat_relay` outlet
through the native Signal Layer. The site asset owns the canonical state, the
climate agent decides, and the dashboard serves the page. The controller is
placed by capability tag, so the same application runs with a simulated sensor
on Linux and with the chip's on-die temperature and a relay on an ESP32-C6.

| Cell | Pattern | Responsibility |
| --- | --- | --- |
| controller | Adapter | Reads `temperature`, drives `heat_relay` |
| site | Asset | Owns the site's canonical state |
| climate-agent | Agent | Decides when to heat, with hysteresis |
| dashboard | Adapter (gateway) | Puts the site on a web page |

## Linux only (no hardware)

The Linux Signal Layer is a separate process, so run it first. It serves the
`temperature` tap from a simulated ramp.

Terminal 1, the pipeline process:

```shell
cd examples/site-control/pipeline
cargo run
```

Terminal 2, the runtime advertising the `signal` capability tag:

```shell
myrmic runtimes start --tag signal
```

Terminal 3, deploy the application and serve the page:

```shell
cd examples/site-control
myrmic deploy app_specs.yml
myrmic gateway
```

Open `localhost:8080/site-control/`. The simulated temperature starts at 20 C,
below the 26 C low target, so the agent asks for heat immediately; it stops
asking above the 30 C high target, and the ramp wraps from 40 C back to 20 C to
repeat. The simulated temperature ignores the heating decision. On Linux there
is no relay, so the controller logs `outlet 'heat_relay': not registered on
this node` and reports `heating_state: false`.

Poke at it from the CLI:

```shell
myrmic subscribe site_state,heating_requested
myrmic send site set_target '{"low": 28, "high": 34}'
myrmic delete site-control --app
```

## ESP32-C6 (on-die temperature and relay)

The board publishes its on-die temperature as the `temperature` tap and drives
the relay on GPIO2. Both are declared by the firmware, so no pipeline or board
file is needed. Flash the `internal-temp` firmware example with your network and
a runtime name:

```shell
export WIFI_SSID='your-network' WIFI_PASS='your-password'
myrmic flash --name esp32c6-runtime embedded/esp-hal/firmware-examples \
    --target internal-temp --monitor
```

If the network blocks multicast scouting, set the runtime's address before
flashing, and the firmware connects to it directly:

```shell
export TCP_DIRECT_ADDR="<linux-ip>:7447"
```

Once the board is on the swarm, give it the tag the controller requires, then
deploy with the Linux runtime untagged so the controller is placed on the board.
`app_specs.esp32.yml` builds the controller for both platforms; the default
`app_specs.yml` is Linux only, so it runs without the embedded toolchain:

```shell
myrmic tags @esp32c6-runtime -t signal
myrmic runtimes delete default
myrmic runtimes start
myrmic deploy app_specs.esp32.yml
myrmic network
```

`heating_state` now follows the relay, and the relay follows the agent's decision.

## What is shared, and what is not

- The four cells are one source each, built for Linux and ESP32-C6.
- The controller's contract with the hardware is the names `temperature` and
  `heat_relay`, not a bus or an address.
- What differs between the two runs is where the tap comes from: a simulated
  ramp on Linux, the on-die sensor on the board.
- State lives with the site asset on the Linux node. The controller holds no
  state; if it is placed elsewhere, the site does not move.
