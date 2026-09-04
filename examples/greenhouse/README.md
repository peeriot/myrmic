# Smart Greenhouse

The complete code for the [Smart Greenhouse tutorial](https://book.myrmic.dev/docs/tutorials/smart-greenhouse):
five cells that keep the soil of a grow bed inside a target moisture range, with
a web dashboard to watch it work. No hardware needed - the sensor is mocked.

| Cell | Pattern | Responsibility |
| --- | --- | --- |
| moisture-sensor | Adapter (mocked) | Reports how wet the soil is, once a second |
| pump | Adapter (actuator) | Waters when told to, and says whether it is running |
| grow-bed | Asset | Knows how the bed is doing and what its plants want |
| irrigation-agent | Agent | The only decision-maker: when to water, and when to stop |
| dashboard | Adapter (gateway) | Puts the bed on a web page |

Run the whole application:

```shell
myrmic runtimes start        # terminal 1
myrmic deploy app_specs.yml  # terminal 2
myrmic gateway               # terminal 3, then open localhost:8080/greenhouse/
```

Poke at it from the CLI:

```shell
myrmic subscribe bed_state,watering_started,watering_stopped
myrmic publish rain 20
myrmic send grow-bed set_target '{"low": 70, "high": 85}'
myrmic delete greenhouse --app
```

The tutorial builds these cells one at a time and explains each piece; start
there if you are reading the code for the first time. Its six parts map onto the
five crates in order.
