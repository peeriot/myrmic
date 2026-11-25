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

One timing rule of that contract is easy to miss: the generated source task runs `init()` and
`sample()` back to back in a single tick, with nothing in between, and treats any error from
`sample()` as a fault that re-runs `init()` on the next tick. A sensor driver that resets or
powers up the device in `init()` must therefore wait or poll for readiness itself (bounded, with
`embassy_time` sleeps under `#[cfg(not(test))]`) before `init()` returns, so the first sample is
a real conversion rather than the sensor's zero image. Reporting "not ready yet" as an error
from `sample()` is wrong: the task would reset the sensor again, every tick.

## Sensors (read side)

| Crate                                       | Device                                        | Outputs                          |
| ------------------------------------------- | --------------------------------------------- | -------------------------------- |
| [`bme280`](bme280/)                         | Bosch BME280 environmental sensor (I²C)       | temperature, pressure, humidity  |
| [`bmp180`](bmp180/)                         | Bosch BMP180 temperature/pressure sensor (I²C)| temperature, pressure            |
| [`ccs811`](ccs811/)                         | ScioSense CCS811 air-quality sensor (I²C)     | eCO₂, TVOC                       |
| [`veml7700`](veml7700/)                     | Vishay VEML7700 ambient light sensor (I²C)    | lux                              |
| [`wsen-itds`](wsen-itds/)                   | Würth WSEN-ITDS 3-axis accelerometer (I²C)    | accel_x, accel_y, accel_z        |
| [`lsm6dso32`](lsm6dso32/)                   | ST LSM6DSO32 6-axis IMU (SPI)                 | accel_x–z, gyro_x–z, temperature |
| [`ads1115`](ads1115/)                       | TI ADS1115 16-bit 4-channel ADC (I²C)         | ain0–ain3                        |
| [`sim-source`](sim-source/)                 | Synthetic deterministic source for HIL tests  | value                            |
| [`spi-loopback`](spi-loopback/)             | SPI loopback test source (MOSI→MISO jumper)   | echo_ok, counter                 |

## Actuators (write side)

| Crate                                                     | Device                                                        | Consumes        |
| --------------------------------------------------------- | ------------------------------------------------------------- | --------------- |
| [`gpio-output`](gpio-output/)                             | Digital on/off output (relay, LED, valve, …)                  | DigitalState outlet |
| [`gpio-output-feedback`](gpio-output-feedback/)           | Digital output with intrinsic contact feedback                | DigitalState outlet (emits `contact`) |
| [`pwm-output`](pwm-output/)                               | PWM duty-cycle output (fan, pump, dimmable LED, …)            | PwmDuty outlet  |
