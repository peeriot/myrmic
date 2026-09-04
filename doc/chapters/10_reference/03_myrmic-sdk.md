---
sidebar_label: Myrmic SDK
---

# Myrmic SDK

The Myrmic SDK is the Rust library for writing cells.

Each page covers one topic: what it is for, the operations it offers, an example, and how it behaves. Use it when you know what you want to do and need the exact operations, behavior, or limits.

## Cell model

- [Cell initialization](03_myrmic-sdk/01_cell-model/01_cell-initialization.md) - Prepare a cell before it starts handling messages.
- [Cell identity and metadata](03_myrmic-sdk/01_cell-model/02_identity-and-metadata.md) - Address a cell by its identity, and read the metadata of each handler invocation.

## Messaging

- [Commands](03_myrmic-sdk/02_messaging/01_commands.md) - Receive and send directed, named messages between cells.
- [Callbacks](03_myrmic-sdk/02_messaging/02_callbacks.md) - Name a handler to receive the answer to a command.
- [Events](03_myrmic-sdk/02_messaging/03_events.md) - Publish named messages and receive them through event handlers.
- [Message encoding](03_myrmic-sdk/02_messaging/04_message-encoding.md) - Encode typed payloads, raw bytes, empty messages, and custom wire formats.

## Scheduling and time

- [Delayed handler invocations](03_myrmic-sdk/03_scheduling-and-time/01_delayed-handler-invocations.md) - Schedule a command handler to run once after a delay and cancel it when required.
- [Periodic handler invocations](03_myrmic-sdk/03_scheduling-and-time/02_periodic-handler-invocations.md) - Invoke a command handler on a recurring period, optionally after an initial delay.
- [Clock, uptime, and pausing](03_myrmic-sdk/03_scheduling-and-time/03_clock-and-wait.md) - Read the current time, read how long a node has been running, and pause a handler.

## Cell lifecycle

- [Cell classes and spawning](03_myrmic-sdk/04_cell-lifecycle/01_cell-classes-and-spawning.md) - Declare child classes and create supervised or detached cells at runtime.
- [Cell termination](03_myrmic-sdk/04_cell-lifecycle/02_cell-termination.md) - Terminate another cell or deliberately stop the current cell.
- [Cell monitoring](03_myrmic-sdk/04_cell-lifecycle/03_cell-monitoring.md) - Receive structured notifications when a supervised child cell is lost.

## State and storage

- [Storage scopes](03_myrmic-sdk/05_state-and-storage/01_storage-scopes.md) - Scope every operation on the runtime database, private to one cell or public and shared.
- [Transient state](03_myrmic-sdk/05_state-and-storage/02_transient-state.md) - Hold a value in the cell's WebAssembly memory for as long as the cell runs.
- [Persistent cell state](03_myrmic-sdk/05_state-and-storage/03_persistent-state.md) - Store, load, mutate, and upsert one typed value that lives in the persistent runtime database.
- [Key-value store](03_myrmic-sdk/05_state-and-storage/04_key-value-store.md) - Store many typed values beneath a shared prefix, and read them by prefix.
- [Table store](03_myrmic-sdk/05_state-and-storage/05_table-store.md) - Keep a named collection of values of one type, each under a key, and read them in key order.
- [Time-series store](03_myrmic-sdk/05_state-and-storage/06_time-series-store.md) - Append timestamped measurements and query samples by series, range, limit, and order.
- [Semantic store](03_myrmic-sdk/05_state-and-storage/07_semantic-store.md) - Run SPARQL update and select queries over RDF triples.
- [Blob store](03_myrmic-sdk/05_state-and-storage/08_blob-store.md) - Store, read, list, rename, and delete binary content held under file-like paths.

## External systems

- [Gateway routes](03_myrmic-sdk/06_external-systems/01_gateway-routes.md) - Expose a swarm to HTTP clients under a URL path, over plain HTTP or a WebSocket.
- [Gateway assets](03_myrmic-sdk/06_external-systems/02_gateway-assets.md) - Serve static files from a cell's blob storage on a gateway route.
- [Bridges](03_myrmic-sdk/06_external-systems/03_bridges.md) - Connect a cell to an MQTT broker or an HTTP service outside the swarm.

## Signal Layer (I/O)

- [Signal taps](03_myrmic-sdk/07_signal-io/01_signal-taps.md) - Read a Signal Layer input by name, either its latest value or its queued events.
- [Signal outlets](03_myrmic-sdk/07_signal-io/02_signal-outlets.md) - Drive a device through a named Signal Layer output, writing a typed value or raw bytes.

## Bluetooth Low Energy

- [BLE scanning and discovery](03_myrmic-sdk/08_bluetooth-low-energy/01_ble-scanning.md) - Scan for peripherals, filter advertisements, and stop an active scan.
- [BLE connections and service discovery](03_myrmic-sdk/08_bluetooth-low-energy/02_ble-connections.md) - Establish a peripheral connection, inspect discovered services, and disconnect explicitly.
- [BLE characteristic reads and writes](03_myrmic-sdk/08_bluetooth-low-energy/03_ble-characteristic-io.md) - Read and write GATT characteristics through a host-managed connection.
- [BLE notifications and subscriptions](03_myrmic-sdk/08_bluetooth-low-energy/04_ble-notifications.md) - Subscribe to GATT notifications, receive payloads through callbacks, and unsubscribe.
- [BLE pairing](03_myrmic-sdk/08_bluetooth-low-energy/05_ble-pairing.md) - Secure a link with a passkey when a peripheral requires it.

## Hardware

- [GPIO](03_myrmic-sdk/09_gpio.md) - Acquire numbered pins, perform digital I/O, and wait for levels or edges.

## Diagnostics

- [Logging](03_myrmic-sdk/10_logging.md) - Log a message at one of five levels.
- [Errors](03_myrmic-sdk/11_error-model.md) - See how a handler fails, what an error carries, and what the runtime rolls back.
