---
sidebar_label: Connected Products
---

# Build the product, not another proprietary device platform

Connected-product teams increasingly have to build connectivity, state, local processing, gateways, hardware integration, and separate stacks for every device class.

They are often forced to choose between portable software that hides valuable hardware differences and hardware-specific software that is difficult to reuse.

## The Myrmic approach

Separate the stable product contract from its native implementation.

```text
portable product logic
WebAssembly Cells
        ↓
Capability contract
        ↓
native implementation for each target
Signal Layer · drivers · optimized Rust
```

Cells contain product state, modes, configuration, messaging, and user-facing behaviour. Native Rust handles drivers, hardware-near processing, diagnostics, and target optimization.

## What the preview demonstrates

- one Cell model across Linux and supported bare-metal Nodes,
- explicit capability tags,
- native Signal Layer implementations,
- placement by capability tags,
- browser and CLI access through the same application interfaces,
- explicit documentation of what happens after Node loss, without implying continuity.

## What the roadmap adds

Partition Tolerance replicates authority and uses fencing to allow one active serving copy. Durability replicates Cell data and lets a Consistent product Cell resume after Node loss.

## Good fits

- connected devices and appliances,
- gateways and controllers,
- building and energy products,
- specialized equipment,
- product families spanning several hardware targets.

## See also

- [Target Groups](../03_target-groups.md) - the other entry points Myrmic supports
- [Understanding Myrmic](../02_why-myrmic/02_understanding-myrmic.md) - the swarm and node model behind this approach
- [Signal Layer](../02_why-myrmic/04_connecting-physical-systems.md) - how Cells relate to native drivers and hardware I/O
