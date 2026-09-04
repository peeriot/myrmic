# Board file

The board file describes one physical board: which chip, which pins carry which bus, and what is
wired to it. It is the only file in the Signal Layer that is not portable.

```yaml
id: esp32c6-devkit
chip: esp32c6

buses:
  i2c0:
    transport: i2c
    pins: { scl: 10, sda: 11 }
    freq_khz: 400
  spi2:
    transport: spi
    pins: { sclk: 19, mosi: 20, miso: 21 }
    freq_khz: 1000

gpios:
  general_purpose: [0, 1, 2, 3, 14, 18, 22, 23]

devices:
  - id: bme280
    driver: bme280
    bus: i2c0
    hardware:
      i2c_addr: 0x76

  - id: loopback
    driver: spi-loopback
    bus: spi2
    pins:
      cs: 22
```

## Top level

| Key | Required | Meaning |
|---|---|---|
| `id` | yes | A name for the board |
| `chip` | yes | Which microcontroller, or `linux` |
| `buses` | yes | Shared connections, keyed by bus id |
| `gpios` | yes | Which pins this board makes available |
| `devices` | yes | What is connected |

## `buses`

| Key | Required | Meaning |
|---|---|---|
| `transport` | yes | `i2c` or `spi` |
| `pins` | yes | Role to GPIO number, e.g. `scl`, `sda`, `sclk`, `mosi`, `miso` |
| `freq_khz` | yes | Bus clock, must be greater than 0 |
| `mode` | no | SPI mode 0-3, SPI only |

The bus id must name a real peripheral: `i2c0` or `i2c1` for I²C, `spi2` or `spi3` for SPI. The
generator accepts all four on any chip, but your chip may not have them all. On the C5, C6 and
C61 there is only one I²C peripheral, so `i2c1` validates and then fails at compile time.

An SPI bus must declare `sclk` and `mosi`. A device on an SPI bus must declare a `cs` pin, which
is the one pin name not taken from the driver's descriptor.

Bus ids name the chip's peripherals on embedded: `i2c0` (and `i2c1` only where the chip has
one), and `spi2` or `spi3` for the user SPI buses. On Linux the id is a free name.

`chip:` accepts `esp32c5`, `esp32c6`, `esp32c61` or `linux`. Any other value fails, with the
list of supported chips named in the error. A chip outside that list cannot be added by
configuration for now.

On Linux, a bus carries a `dev_path` instead of pins: `/dev/i2c-1` for I²C, `/dev/spidev0.0` for SPI (with `pins: {}`). An SPI device's `pins.cs` stays a GPIO line number, driven by the Signal Layer itself.

## `gpios.general_purpose`

The pins this board makes available for non-bus use.

Two rules, both enforced:

- **Bus pins must not appear here.** They are declared under `buses` and reaching them through
  this list too is an error.
- **Every pin a device claims must appear here.** A device draws its pins from this set.

Pins outside the chip's known layout are rejected. Note the layout is the set of pins the runtime
can expose, which is narrower than the chip's full GPIO count, so some otherwise usable pins
cannot carry a device.

Anything listed here and not claimed by a device is offered to cells, which address pins by their
real GPIO number.

## `devices`

| Key | Required | Meaning |
|---|---|---|
| `id` | yes | Name the pipeline uses to refer to this device |
| `driver` | yes | Directory name from the [drivers reference](./03_drivers.md) |
| `bus` | no | Bus id; omit for a device driven straight from GPIO |
| `pins` | no | Role to GPIO number, using the roles the driver declares |
| `hardware` | no | Settings the driver declared as `scope: hardware` |

Pin role names must be declared in the driver's descriptor, except `cs` on an SPI bus. A GPIO may
be claimed by only one device.

`id` and `driver` must both be valid Rust identifiers, as must every bus id.

## Where a setting goes

Each driver marks every setting it accepts with a scope, and the scope decides the file:

| Scope | Belongs in | Typical example |
|---|---|---|
| `hardware` | this file, under `hardware:` | an I²C address fixed by a solder bridge |
| `application` | the pipeline, under `config:` | how often to sample |

Putting one in the wrong file is rejected, and the message names the field. A key that matches no
setting at all is silently ignored, so a typo in a setting name is not reported.
