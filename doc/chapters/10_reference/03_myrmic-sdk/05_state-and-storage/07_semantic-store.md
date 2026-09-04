# Semantic store

> **Availability:** Linux and embedded runtimes

The semantic store keeps data as RDF triples: a subject, a predicate, and an object. A cell writes and reads them with SPARQL.

## Operations

- Write and update triples.
- Read triples.

## Example

```rust
use myrmic_sdk::db::{Scope, sem_select, sem_update};
use myrmic_sdk::Metadata;

const ASSETS: Scope = Scope::public("assets");

#[myrmic_sdk::cmd]
fn record_status(_md: Metadata) -> myrmic_sdk::Result {
    sem_update(
        ASSETS,
        "INSERT DATA { <urn:sensor:1> <urn:status> <urn:active> . }".into(),
        // No base, so every IRI has to be absolute.
        None,
    )
    .map_err(|_| "the update failed")?;

    let response = sem_select(
        ASSETS,
        "SELECT ?sensor ?status WHERE { ?sensor <urn:status> ?status . }".into(),
        None,
        Some(10),
        None,
    )
    .map_err(|_| "the query failed")?;

    for solution in &response.solutions {
        for (variable, value) in response.variables.iter().zip(solution) {
            // Some rows may have no value for a selected variable.
            if let Some(value) = value {
                // variable is "sensor" or "status".
                myrmic_sdk::info!("{variable} = {value}")?;
            }
        }
    }

    Ok(())
}
```

## Behavior

### Normal

An update or a select runs in the scope it names. An update takes effect when the handler returns successfully, together with everything else it wrote, and is rolled back if the handler fails.

A select returns at most 100 rows unless it asks for more.

### Errors

A query fails when the SPARQL is invalid, a read is not a select, an IRI is bad, storage is unavailable, or the result is too large to fit. A query too long to encode fails before the runtime is called.

### Limits

The query text has to fit in 1000 bytes. A select's rows have to fit in 1000 bytes too. Anything larger fails.

For now an update can only insert triples, delete them, do both at once, or create a graph.

## API documentation

For exact signatures and the response type, see [`sem_update`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/db/fn.sem_update.html), [`sem_select`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/db/fn.sem_select.html) and [`SelectResponse`](https://docs.myrmic.intra/myrmic_sdk/git/myrmic_sdk/db/struct.SelectResponse.html).
