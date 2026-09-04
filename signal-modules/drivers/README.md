# Drivers

One crate per hardware peripheral. A driver is a small `#![no_std]` crate that talks to a
device over a bus (I²C, SPI, GPIO, …) and either **reads** it into tap slots (a sensor) or
**writes** to it from an outlet slot (an actuator). Each crate ships a `descriptor.yaml` that
codegen reads to wire the driver into a pipeline.

Drivers never own their bus and never touch the tap registry directly — codegen hands them a
shared-bus device and the slot handles they need. For the driver contract, the descriptor
schema, and a full walkthrough, see
[the driver guide](../../doc/chapters/05_guides/11_signal-layer/07_write-your-own-driver.md) (and the parent
[`../README.md`](../README.md) for how drivers fit the drivers → steps → taps model).

## Sensors (read side)

| Crate                                       | Device                                        | Outputs                          |
| ------------------------------------------- | --------------------------------------------- | -------------------------------- |
| [`bme280`](bme280/)                         | Bosch BME280 environmental sensor (I²C)       | temperature, pressure, humidity  |
| [`bmp180`](bmp180/)                         | Bosch BMP180 temperature/pressure sensor (I²C)| temperature, pressure            |
| [`ccs811`](ccs811/)                         | ScioSense CCS811 air-quality sensor (I²C)     | eCO₂, TVOC                       |
| [`veml7700`](veml7700/)                     | Vishay VEML7700 ambient light sensor (I²C)    | lux                              |
| [`wsen-itds`](wsen-itds/)                   | Würth WSEN-ITDS 3-axis accelerometer (I²C)    | accel_x, accel_y, accel_z        |
| [`ads1115`](ads1115/)                       | TI ADS1115 16-bit 4-channel ADC (I²C)         | ain0–ain3                        |
| [`sim-source`](sim-source/)                 | Synthetic deterministic source for HIL tests  | value                            |

## Actuators (write side)

| Crate                                                     | Device                                                        | Consumes        |
| --------------------------------------------------------- | ------------------------------------------------------------- | --------------- |
| [`gpio-output`](gpio-output/)                             | Digital on/off output (relay, LED, valve, …)                  | DigitalState outlet |
| [`gpio-output-feedback`](gpio-output-feedback/)           | Digital output with intrinsic contact feedback                | DigitalState outlet (emits `contact`) |
| [`pwm-output`](pwm-output/)                               | PWM duty-cycle output (fan, pump, dimmable LED, …)            | PwmDuty outlet  |
