# Documentation tools

## `gen-signal-layer-reference.py`

Regenerates the driver and step reference pages from the module descriptors:

```
python3 doc/tools/gen-signal-layer-reference.py     # run from the repo root
```

It reads every `signal-modules/{drivers,steps}/*/descriptor.yaml` and writes
`doc/chapters/10_reference/04_signal-layer/03_drivers.md` and `04_steps.md`.

Both pages carry a generated-file banner. Edit the descriptors, then re-run this; do not edit the
pages by hand. Adding a driver or a step needs no change here.
