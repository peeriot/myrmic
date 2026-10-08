# Modbus TCP Remote I/O

A cell that switches a fan by a temperature, through the remote I/O of a
Weidmüller u-remote UR20 coupler: a PT100 on an RTD module measures, a fan on
a DO module cools, and a heating foil on the same module warms for a minute at
the push of a button on a DI module. Without the hardware, a simulator stands
in for the coupler.

| Part | What it is | Responsibility |
| --- | --- | --- |
| ur20-simulator | Host program | A UR20 coupler with the modules below, and the air around the PT100 |
| ur20-modbus-tcp-bridge.yml | Modbus bridge | Polls the temperature, the heat button, the module list and the module parameters, switches the fan and the heating foil, sets the module parameters |
| temperature-controller | Cell | Switches the fan on above 28 °C and off below 26 °C, the heating foil by button or command for 60 s at most and off above 45 °C, sets the RTD channel to PT100 and the input delay of the button, warns about unexpected modules |

| Slot | Module | Channel 0 | Channel 1 |
| --- | --- | --- | --- |
| 0 | UR20-4DI-P | heat button | - |
| 1 | UR20-4AI-RTD-DIAG | PT100 | - |
| 2 | UR20-4DO-P-2A | fan | heating foil |

| Table | Address | Value |
| --- | --- | --- |
| discrete input | 0x0000 | heat button, `true` while pressed |
| input register | 0x0001 | temperature, `i16` in 0.1 °C |
| coil | 0x8000 | fan |
| coil | 0x8001 | heating foil |
| holding register | 0x2A02 | module in slot 1, `u32` |
| holding register | 0x2A04 | module in slot 2, `u32` |
| holding register | 0xC000 | input delay of the heat button, `3` is 10 ms |
| holding register | 0xC101 | measurement range of the RTD channel, `0` is PT100 |
| holding register | 0xC102 | connection type of the RTD channel, `0` is two-wire |

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

Type `b` to press the heat button: the heating foil warms the PT100 until the
fan starts, too. The cell switches it off again after 60 s, at the next press,
or above 45 °C. The same from the CLI:

```shell
myrmic send temperature-controller heat '{"on": true}'
myrmic send temperature-controller heat '{"on": false}'
```

When the cell starts, it switches the heating foil off: the coupler keeps its
outputs across a restart of the cell.

## On the real coupler

If the coupler has another address than its factory one, change `host` in
`ur20-modbus-tcp-bridge.yml`.

The coupler forgets module parameters on a power cycle unless they are saved
in its web server. So the cell sets channel 0 of the RTD module to PT100,
two-wire, and the input delay of the heat button to 10 ms, within ten seconds
whenever it finds them otherwise. The module parameters of slot `n` start at
register `0xC000 + n * 0x100`.

The input delay keeps the bouncing of the heat button at the coupler. The
bridge polls the button every 50 ms and passes on only its changes, and the
cell acts on the press, not on the level: it needs no copy of the button,
only of the heating foil. A press shorter than 50 ms may go unnoticed.

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
