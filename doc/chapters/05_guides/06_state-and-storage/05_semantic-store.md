# Semantic Store

The semantic store is an [RDF](https://www.w3.org/TR/rdf11-concepts/) triple store, written and queried with [SPARQL](https://www.w3.org/TR/sparql11-query/). The SDK does not provide an abstraction layer for it - all interactions happen directly through the functions below.

## Update

SPARQL updates modify the store - they can insert new data, delete existing entries, or both. The SDK provides `sem_update`. It takes a scope, a SPARQL update query, and an optional base IRI:

### Insert

This example inserts three temperature sensors into the semantic store, each assigned to a zone in a building.

```rust
use myrmic_sdk::db::{sem_update, Scope};

let query = r#"
    PREFIX ex: <http://myrmic/example/>
    INSERT DATA {
        ex:sensor_01 a ex:TemperatureSensor ;
                    ex:assignedTo ex:zone_a ;
                    ex:status ex:Active .

        ex:sensor_02 a ex:TemperatureSensor ;
                    ex:assignedTo ex:zone_b ;
                    ex:status ex:Active .

        ex:sensor_03 a ex:TemperatureSensor ;
                    ex:assignedTo ex:zone_a ;
                    ex:status ex:Inactive .

        ex:zone_a a ex:Zone ;
                 ex:partOf ex:building_01 .

        ex:zone_b a ex:Zone ;
                 ex:partOf ex:building_01 .

        ex:building_01 a ex:Building .
    }
"#;

sem_update(Scope::default(), query.into(), None)?;
```

### Delete

This example removes all triples about `sensor_03` from the store.

```rust
use myrmic_sdk::db::{sem_update, Scope};

let query = r#"
    PREFIX ex: <http://myrmic/example/>
    DELETE WHERE {
        ex:sensor_03 ?p ?o .
    }
"#;

sem_update(Scope::default(), query.into(), None)?;
```

## Select

SPARQL SELECT queries read from the store. Querying is possible through `sem_select` - it takes a scope and a SPARQL SELECT query, with optional base IRI, limit, and skip, and returns the matching results.

This example queries for active temperature sensors in `building_01`, returning each sensor and the zone it is assigned to:

```rust
use myrmic_sdk::db::{sem_select, Scope};

let query = r#"
    PREFIX ex: <http://myrmic/example/>
    SELECT ?sensor ?zone WHERE {
        ?sensor a ex:TemperatureSensor ;
                ex:status ex:Active ;
                ex:assignedTo ?zone .
        ?zone ex:partOf ex:building_01 .
    }
"#;

let response = sem_select(
    Scope::default(),
    query.into(),
    None,       // base IRI - provide when using relative IRIs instead of PREFIX declarations
    Some(10),   // limit
    None,       // skip
)?;

for solution in response.solutions {
    for (variable, value) in response.variables.iter().zip(solution.iter()) {
        if let Some(val) = value {
            // variable = "sensor" or "zone"
        }
    }
}
```

`sem_select` returns results as `SelectResponse`. It holds:
- `variables` - the names of the selected variables
- `solutions` - one row per result, values in the same order as `variables`. `None` means no value was found for that variable.

For the query above, the response would look like:

```rust
SelectResponse {
    variables: vec!["sensor", "zone"],
    solutions: vec![
        vec![Some("http://myrmic/example/sensor_01"), Some("http://myrmic/example/zone_a")],
        vec![Some("http://myrmic/example/sensor_02"), Some("http://myrmic/example/zone_b")],
    ],
}
```

Both `sem_update` and `sem_select` take an optional `base_iri` parameter - the base IRI for relative IRIs in the query. Pass `None` to skip it. See [Relative IRIs](https://www.w3.org/TR/sparql11-query/#relIRIs).

## See also

- [How to work with state and storage](../06_state-and-storage.md#semantic-store) - back to the guide
- [Time-series store](./04_time-series-store.md) - timestamped measurements

## Related SDK reference

- [Storage scopes](../../10_reference/03_myrmic-sdk/05_state-and-storage/01_storage-scopes.md)
- [Semantic store](../../10_reference/03_myrmic-sdk/05_state-and-storage/07_semantic-store.md)
