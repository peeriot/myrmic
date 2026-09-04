## Benchmarks

The crate ships a `criterion` bench suite under `benches/` covering domain ops, SPARQL, replication apply, and transaction overhead.

Run everything:

```
cargo bench -p db
```

Run a single surface:

```
cargo bench -p db --bench domain
cargo bench -p db --bench semantic
cargo bench -p db --bench replication
cargo bench -p db --bench tx
```

Filter by bench name:

```
cargo bench -p db --bench domain -- kv_
```

Benches are not wired into CI; treat them as a local diagnostic tool until baselines settle.

## Durability tests

A process-kill harness (`tests/durability/`) spawns node workers as child processes, brokers replication between them, SIGKILLs them mid-flight, and checks the cluster recovers and re-converges. Gated behind `DB_DURABILITY=1`; a plain `cargo test -p db` skips it.

Run all scenarios:

```
DB_DURABILITY=1 cargo test -p db --test durability
```

Run one scenario — `kill_during_write_burst`, `kill_receiver_mid_catchup`, `kill_sender_mid_catchup`, `crash_loop`, `kill_during_gc`:

```
DB_DURABILITY=1 cargo test -p db --test durability -- kill_during_write_burst
```

`SEED=n` replays a specific run; `STRESS_SECS=n` adds (and sizes) the randomized `stress` scenario:

```
DB_DURABILITY=1 SEED=42 cargo test -p db --test durability -- crash_loop
DB_DURABILITY=1 STRESS_SECS=300 SEED=42 cargo test -p db --test durability -- stress
```

On failure the harness prints the seed and keeps each node's data dir + `<node>.log`. Inspect a kept dir's sync points and keys:

```
DB_DURABILITY=1 cargo test -p db --test durability -- inspect <data-dir> <namespace>
```
