# Signal Layer

Reference for the Signal Layer: the two files that describe a system, and the modules they can
name.

- [Pipeline file](./04_signal-layer/01_pipeline-file.md) - sources, steps, taps and outlets,
  the payload types they can carry, and the limits.
- [Board file](./04_signal-layer/02_board-file.md) - chip, buses, pins and devices, and which
  settings belong here rather than in the pipeline.
- [Drivers](./04_signal-layer/03_drivers.md) - every driver in the tree, what it reads or
  writes, and what it can be configured with.
- [Steps](./04_signal-layer/04_steps.md) - every processing step, its input and output types,
  and its configuration.
- [Running on Linux](./04_signal-layer/05_running-on-linux.md) - the pipeline process, the
  socket, and what differs from embedded.

For how to use any of it, see the guide: [Work with the Signal
Layer](../05_guides/11_signal-layer.md).

The driver and step pages are generated from the module descriptors, so they cannot drift from
what is in the tree.
