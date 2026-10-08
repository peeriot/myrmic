# Modbus TCP Boiler

A cell that keeps the water of a boiler around its setpoint, talking to the
boiler over Modbus TCP through a Modbus bridge. No hardware needed - the
boiler is simulated.

| Part | What it is | Responsibility |
| --- | --- | --- |
| boiler-simulator | Host program | A boiler that speaks Modbus TCP: water temperature, burner, setpoint |
| boiler-modbus-tcp-bridge.yml | Modbus bridge | Polls the temperature and the burner, reads and writes the setpoint, switches the burner |
| boiler-controller | Cell | Switches the burner so the water stays within 2 °C of the setpoint |

The simulated boiler maps:

| Table | Address | Value |
| --- | --- | --- |
| input registers | 0x0100, 0x0101 | water temperature in °C, `f32`, `abcd` |
| coil | 0x0005 | burner on/off |
| holding register | 0x0200 | setpoint in °C, `i16`, 60 at start |

While the burner is on the water heats up by 1 °C per second, while it is off
it cools down by 0.3 °C per second.

Run the whole application:

```shell
cargo run -p boiler-simulator  # terminal 1, from examples/
myrmic runtimes start          # terminal 2
myrmic deploy app_specs.yml    # terminal 3, from examples/modbus-tcp-boiler/
```

The simulator logs every switch of the burner, the runtime the log of the
boiler controller:

```text
[2026-09-29T14:52:10Z INFO  boiler_simulator] burner on at 15.0 °C
[2026-09-29T14:53:00Z INFO  boiler_simulator] burner off at 63.0 °C
```

`RUST_LOG=warn` keeps the simulator quiet but for failed connections.

Poke at it from the CLI. Change the setpoint through the controller: it writes
the new value to the boiler and regulates to it. It reads the setpoint from
the boiler only once, so a value written to the boiler directly would go
unnoticed.

```shell
myrmic subscribe boiler_temperature
myrmic send boiler-controller set_setpoint '{"setpoint": 70}'
myrmic delete modbus-tcp-boiler --app
```

The simulator listens on `127.0.0.1:5020`, so the runtime has to run on the
same machine. To run it elsewhere, pass an address - `cargo run -p
boiler-simulator -- 0.0.0.0:5020` - and point `host` in
`boiler-modbus-tcp-bridge.yml` at that machine.

To learn how the bridge and the cell fit together, see the
[Modbus guide](https://book.myrmic.dev/docs/guides/modbus).
