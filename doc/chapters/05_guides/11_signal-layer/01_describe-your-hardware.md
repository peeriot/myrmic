# Describe your hardware

The board file is where you write down what your hardware actually is: which chip, which
pins carry which bus, what is wired to it, and where each device sits. This page explains
how to write one, and how to work the facts out for a board nobody has described before.

Everything else you write is portable. A pipeline moves from a devkit to a custom product
without changing, and a cell never knows what it is running on. The board file is the one
place where that stops being true, because it is the only file that describes a physical
object.

## Two ways in

There are two honest ways to write this file, and which one you want depends on what you
are describing.

If you are describing **a board**, such as a devkit or something you expect to reuse
across projects, work subtractively. Open the chip's datasheet and the board's pinout, and strike
out every pin that is spoken for or unsafe. What survives is the board's real capability,
and you write it down once.

If you are describing **a product**, a design whose wiring is fixed and will not change,
work additively. You already know what is connected, because you decided it. Add the pins
you actually use. Pins nobody wired cannot be used anyway, so a shorter list costs you
nothing.

Neither is more correct. The subtractive pass takes longer and produces a file that
outlives the project; the additive one gets you running today.

## The four things you write

A board file has four parts, and they build on each other. Here is a complete one, for a
board with a temperature sensor on I²C and a relay on a bare pin.

```yaml
id: my-board
chip: esp32c6

buses:
  i2c0:
    transport: i2c
    pins:
      scl: 10
      sda: 11
    freq_khz: 400

gpios:
  general_purpose: [2, 3, 18, 19, 20, 21, 22, 23]

devices:
  - id: bme280
    driver: bme280
    bus: i2c0
    hardware:
      i2c_addr: 0x76   # SDO pulled to GND

  - id: relay1
    driver: gpio-output
    pins:
      out: 2
    hardware:
      active_low: false
```

`id` is a name for the board, and `chip` says which microcontroller it is built around.
`chip` is not decoration: it decides which pins exist at all.

`buses` declares each shared connection and the pins that carry it. The bus name is not
free-form: it has to name a real peripheral, so I²C buses are `i2c0` or `i2c1` and SPI buses
`spi2` or `spi3`. The generator accepts those four names on any chip, but your chip may not have
all of them: the C5, C6 and C61 each expose only one I²C peripheral, so `i2c1` passes validation
and then fails when the firmware is compiled. Check your chip before picking the second one.

`devices` lists what is connected. Each entry names an `id` you choose, the `driver` that
knows how to talk to it, and either the `bus` it hangs off or the pins it is wired to
directly. A device with no `bus` is driven straight from GPIO, like the relay above.

`gpios.general_purpose` lists the pins available for non-bus use. The two rules around it
pull in opposite directions, and both are checked. Bus pins are already declared under
`buses:`, so they must not be repeated here: GPIO10 and GPIO11 are absent from the list
above for that reason. A device's own pins are the other way round. `relay1` claims GPIO2,
and that claim is only valid if GPIO2 appears in this list, so a device draws its pins from
the general-purpose set rather than from outside it.

## Where the facts come from

Three facts have to come from somewhere, and only one of them is visible on the board.

**Which pin is which** comes from the board's pinout diagram, or the schematic if the board
is your own. Silkscreen is convenient and usually right, but it is not authoritative. Plenty
of boards label sparsely, or not at all.

**Which pins you must not touch** comes from the chip's datasheet or technical reference
manual. This is the subtractive pass, and it is the part worth taking slowly.

**A device's address** comes from its own datasheet, and then from how your particular board
strapped it. Some devices have a fixed address and there is nothing to decide. Others have
configuration pins, and the value depends on whether they were pulled high or low; breakout
boards commonly encode this as a solder bridge. This is why the example above carries a
comment beside the address. Six months later, `0x76 # SDO pulled to GND` tells you why,
and `0x76` alone does not.

## Which pins are actually free

The list of pins you can use is always shorter than the list the chip has, and the reasons
are not visible in the pin numbers. Your chip's datasheet is the authority. What follows is
the kind of thing to look for, not a list that will come out the same on your board.

Some pins are genuinely not yours. Your chip reaches its flash over a small group of pins,
and on parts with PSRAM the same group usually carries that too. These are ordinary
GPIO-numbered pins and the chip will let you configure them, but your firmware is executing
out of that flash while it runs, so repurposing them stops the program doing the
repurposing. Espressif's tooling reports them as RESERVED for exactly this reason. Which
numbers they are differs from chip to chip, so look yours up rather than carrying a range
over from another part.

Others are a trade-off rather than a prohibition, and this is worth deciding rather than
assuming. A peripheral such as USB, or the JTAG debug interface, occupies particular pins
for as long as you want that function. If your product does not expose USB, or you debug
over something else, those pins are yours to take, and on a pin-starved design that can be
the right call. The question is which you need more: the function, or the pins.

Some pins are sampled at reset to decide how the chip boots. Those are often usable
afterwards, provided whatever you attach does not hold them at the wrong level while the
chip is coming up.

And some are simply not brought out to a header on your particular board, which is a fact
about the board rather than about the chip.

Make the trade-off deliberately, because arriving at it by accident is unpleasant. Take the
pins the USB peripheral uses without meaning to, and the board stops enumerating over USB
the moment your firmware runs, which also means you can no longer flash it the usual way. It
looks exactly like a bricked board. It is not: hold the boot pin low while resetting, which
on most devkits is a button, and the chip comes up in its bootloader with your firmware not
running, at which point you can flash something else. If you took those pins on purpose,
that same procedure is simply how you flash the board from now on.

A pin left out of `general_purpose` cannot be claimed by a device and is not offered to cells.
Bus pins are the exception, and must be left out precisely because the bus declaration reaches
them instead.

## Which settings belong here

A device's settings are split between this file and the pipeline, and the split is not a
matter of taste.

If a setting is decided by the physical hardware (an address fixed by a solder bridge, a
relay that is wired active-low, the frequency a bus runs at), it belongs here, in the
device's `hardware:` block. Changing it would mean changing the board.

If a setting could change without touching a wire (how often to sample, a threshold, a
smoothing window), it belongs in the pipeline.

Drivers declare which of their settings is which, and the rule is enforced rather than
suggested: put a setting in `hardware:` that the driver marked as belonging to the
application, and generation fails with a message naming the field. You cannot quietly get
this wrong.

## Pins you do not claim

A pin listed in `general_purpose` and not claimed by any device is handed to cells, which
can drive it directly through the GPIO interface. Cells address pins by their real number,
so a cell asking for `11` means GPIO11.

This is a deliberate escape hatch, not the main road. The reason the Signal Layer exists is
that drivers own hardware and cells consume values; a cell toggling a pin itself is for
bring-up, a status LED, a one-off. Reach for a driver first.

It is safe by construction. A pin claimed by a device is not merely discouraged for cells.
It is not offered to them at all, and the generated pipeline moves each exposed pin out of
the peripheral set, so a build where both sides tried to own the same pin would not compile.
You cannot create a conflict here, whatever you write.

What you *can* do is tell the truth badly. `general_purpose` is a claim about your board
that nothing is able to check, because no tool knows what you soldered. List a pin that is really
wired to a relay you never described as a device, and you have handed that relay to any
cell that asks for it. That is the one mistake in this file with no safety net.

## A board nobody has described yet

Most boards do not have a description yet, and writing one is usually not a porting effort.
How much work it takes depends on the chip, not on the board.

If the chip is already supported, no code is involved at all. Everything above is all there
is to it: read the pinout, work out which pins are free, declare your buses, list your
devices. A new board on a familiar chip is pure configuration, and this is the common case.

If the chip is one Myrmic already runs on but the Signal Layer has not been taught,
its pin layout needs adding, because `chip` selects a table of which pins exist. That is a
small, contained change.

If Myrmic does not run on the chip at all, that is a real port, and it reaches well
beyond this file: the hardware abstraction layer, the radio and scheduler support, the
watchdog, storage. Writing the board file is the last step of that work rather than the
first.

## When you get it wrong

Mistakes in this file surface in three quite different ways, and it helps to know which kind
you are looking at.

**Mistakes the generator can see**, it catches before anything is built: a bus pin repeated
in the general-purpose list, a device pin missing from it, two devices claiming the same pin,
a source naming a device that does not exist, a pin name the driver never declared, a bus id
the chip does not have, a pin outside the chip's layout, a setting in the wrong place. These
come with specific messages, and they are the easy ones.

**Mistakes about a device on a bus** are usually caught by the driver, because a good driver
checks it is talking to what it expects. The BME280 driver reads the chip's identity register
before anything else, and rejects a response that is not the BME280's. A wrong address, a swapped
SDA and SCL, or a sensor that is not powered all land here.

You will see this once:

```
[bme280] init failed — sensor Down
```

and then nothing more. The source keeps retrying quietly at its sample period, so the message is
not repeated while it stays down. Note what the line does *not* tell you: the driver's own reason
is discarded, so you learn that bring-up failed but not why. The name in brackets is the source
id from your pipeline, which matches the device name only if you chose the same one. Its tap
stays empty rather than holding a stale value, and the pipeline reports the device as down. If it starts working, because you reseat a wire or fix the address,
it recovers on its own and says so. The important part is that the *explanation* is printed
once, at startup. Miss it and all you have is a tap with nothing in it.

**Mistakes about an actuator are not caught at all.** A relay driver told to drive the wrong
pin drives the wrong pin, successfully, forever. There is nothing to read back and no error
to raise. Sensors can self-diagnose because they can ask a device who it is; a bare output
cannot. This is exactly why a feedback-capable output driver exists: it reads the real state
from a separate input line rather than inferring it from the value it just wrote. If
knowing an actuator's true state matters, that is the driver to reach for. Otherwise your
only instrument is the hardware itself.
