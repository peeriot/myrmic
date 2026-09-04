# cell-ctl Tutorial

This tutorial walks through starting a swarm and using
cell-ctl to manage cells. All commands assume you are running from the
`swarm/tutorials/cell-ctl/` directory.

## Setup

Run the setup script to build all required binaries and move them into the
tutorial workspace:

```sh
./setup.sh
```

## Start the swarm

Launch the tutorial layout with zellij:

```sh
zellij --layout ./layout.kdl
```

This opens two panes: the swarm runtime on the left and a shell in the
tutorial workspace on the right. Wait for the runtime to finish starting up.

## Check cell status

In the cell-ctl pane, list all registered cells:

```sh
./cell-ctl status
```

You should see no cells at this point.

```sh
./cell-ctl status <sri>
```

## Deploy the room cell

The room cell's SRI must be `room_cell` — the thermostat module has this
value hardcoded as the target for its commands.

```sh
./cell-ctl deploy room_cell --wasm ./room.wasm
```

The tool will upload the wasm binary to the datalayer if it is not already
present, then deploy the cell. Verify that it is registered:

```sh
./cell-ctl status
```

You should now see the room cell.

## Deploy the thermostat cell

The thermostat cell wraps the room cell — it accepts string commands and
delegates to the room cell internally. Deploy it:

```sh
./cell-ctl deploy my_thermostat --wasm ./thermostat.wasm
```

Verify both cells are registered:

```sh
./cell-ctl status
```

## Send commands

Set the room temperature via the thermostat:

```sh
./cell-ctl command my_thermostat --name set_room_temperature --payload "25"
```

The thermostat sets the temperature on the room cell and returns the confirmed
value as a string. You should see `25` in the response.

### A note on domain-type cells

You can also query the room cell directly:

```sh
./cell-ctl command room_cell --name get_temperature
```

Since the room cell uses binary-encoded domain types (not strings), cell-ctl
will display the raw bytes rather than a readable value. Cells that use
domain-specific binary encoding are better accessed through wrapper cells (like
the thermostat) or programmatic clients. The CLI's raw bytes display serves as
a debugging aid.

## Delete a cell

Remove the thermostat cell:

```sh
./cell-ctl delete my_thermostat
```

Verify it is gone:

```sh
./cell-ctl status
```

You should see only the room cell. The room cell remains
available since it was deployed independently.

## Cleanup

After you are done, run the cleanup script from the tutorial directory:

```sh
cd ..
./cleanup.sh
```
