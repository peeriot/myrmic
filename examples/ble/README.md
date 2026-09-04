# BLE adapter examples

This folder collects example cells that read real Bluetooth Low Energy sensors
and publish their measurements as events. There is one adapter per device, each a
self-contained crate.

| Adapter | Device | Measurements |
| --- | --- | --- |
| adapter-govee-h5075 | Govee H5075 thermo-hygrometer | Temperature, humidity, battery |
| adapter-thermobeacon-mini | Thermobeacon Mini, also sold as Thermoplus, Brifit and Oria | Temperature, humidity, battery voltage |
| adapter-switchbot-co2-pro | SwitchBot CO2 Sensor Pro | Temperature, humidity, CO2, battery |
| adapter-ruuvitag-pro | RuuviTag Pro | Temperature, humidity, pressure, battery |
| adapter-htp-xw | SensorPush HTP.xw | Temperature, humidity, barometric pressure |

The devices were chosen to be different from each other rather than
representative. Some broadcast their readings to anyone listening, others hand
over nothing until a connection is opened, and one has to be asked for every
individual sample. Between them the five adapters show what talking to a BLE
sensor from a cell looks like across that range, so a new device can start from
whichever one behaves most like it.

Each adapter documents its own device at the top of its `lib.rs`: what it
publishes, how to build and drive it, and the protocol references it was written
against.
