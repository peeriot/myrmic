# Modbus TCP Remote I/O

A cell that switches a fan by a temperature, through the remote I/O of a
Weidmüller u-remote UR20 coupler: a PT100 on an RTD module measures, a fan on
a DO module cools. Without the hardware, a simulator stands in for the coupler.

| Part | What it is | Responsibility |
| --- | --- | --- |
| ur20-simulator | Host program | A UR20 coupler with the modules below, and the air around the PT100 |
| ur20-modbus-tcp-bridge.yml | Modbus bridge | Polls the temperature and the module list, switches the fan |
| fan-controller | Cell | Switches the fan on above 28 °C and off below 26 °C, warns about unexpected modules |

| Slot | Module | Channel 0 |
| --- | --- | --- |
| 0 | UR20-4DI-P | - |
| 1 | UR20-4AI-RTD-DIAG | PT100 |
| 2 | UR20-4DO-P-2A | fan |

| Table | Address | Value |
| --- | --- | --- |
| input register | 0x0001 | temperature, `i16` in 0.1 °C |
| coil | 0x8000 | fan |
| holding register | 0x2A02 | module in slot 1, `u32` |
| holding register | 0x2A04 | module in slot 2, `u32` |

The bridge talks to the coupler at its factory address `192.168.0.222`, port
502. For the simulator, set `host: 127.0.0.1` and `port: 5020` in
`ur20-modbus-tcp-bridge.yml`, then run the whole application:

```shell
cargo run -p ur20-simulator     # terminal 1, from examples/
myrmic -v runtimes start --tmp  # terminal 2
myrmic deploy app_specs.yml     # terminal 3, from examples/modbus-tcp-remote-io/
```

With `-v` the runtime shows every Modbus request with its answer and every
temperature the cell receives. Type `h` into the simulator to take the PT100 in
the hand: it warms towards 34 °C until the fan starts. Type `h` again to let it
go.

```text
[2026-10-08T15:37:19Z INFO  ur20_simulator] fan on at 28.2 °C
[2026-10-08T15:37:26Z INFO  ur20_simulator] PT100 let go at 26.2 °C
[2026-10-08T15:37:27Z INFO  ur20_simulator] fan off at 25.5 °C
```

## On the real coupler

If the coupler has another address than its factory one, change `host` in
`ur20-modbus-tcp-bridge.yml`. In the coupler's web server, set channel 0 of
the RTD module to PT100, three-wire.

The coupler packs the inputs and the outputs of its modules one after another
in slot order, from register 0x0000 and 0x0800; bit `b` of register `r` is coil
`r * 16 + b`. So the addresses above hold only for the modules above. If yours
differ, the cell warns within ten seconds:

```text
WARN  found module 0x00091f84 where UR20-4AI-RTD-DIAG (0x04061544) is expected: the addresses in ur20-modbus-tcp-bridge.yml do not fit the coupler
```

Then move the addresses to where your coupler has the modules; its web server
shows the process image.

Mind the Modbus watchdog: it switches the outputs off when no request comes in
time.
