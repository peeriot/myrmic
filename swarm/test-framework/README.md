# Myrmic e2e tests

---
Prebuild binary of `myrmic` is required. 
---

Writing a myrmic e2e you will need to use the `test_framework::myrmic::Myrmic` type. You have two
option on how to use the type:
- local binary - `Myrmic::local()`
- binary inside a docker container - `Myrmic::attach("my-container-id)`

Once initialized the `Myrmic` gives a common interface, not matter if running locally or inside a 
docker container. 

In any case the first thing you will be doing is starting a runtime: 
```rust
let myrmic = Myrmic::local();
let runtime = myrmic.start_runtime("my-runtime").await
// ...
runtime.delete().await;
```



The next thing you will usually do is deploying a cell or application:
```rust
// crate new cell and deploy
let cell_spec = myrmic.new_cell("my-cell", None).await;
let cell = myrmic.deploy(cell_spec, "my-cell-sri").await;
//...
cell.delete().await;


// deploy an application specification
myrmic.deploy_app("assets/apps/app_spec.yml").await;
```

After cells or am application have deployed, you can interact with those. For single cells the 
returned `DeployedCell` offers a `send` function that automatically uses the correct SRI. For 
applications with multiple cells the `Myrmic` type also offers a `send` function that needs to know 
the SRI you are sending to.


---
**NOTE**

In case a test failed and a runtime with that name exist, the `start_runtime` command panics, same
applies to deploy commands with existing cell SRIs. It is very important that runtimes are deleted.
---


# Network Tests

---
Prebuild binaries of `swarm`, `test-sidecar` are required. 
---

Network tests always run inside containers and use network shaping to simulate package loss for 
example. The basic structure of such tests can be described as docker compose files and usually
have some preconditions:

```rust
// initialize docker client
let docker = init_docker();

// build the sidecar image
Sidecar::build(
    &docker,
    "assets/dockerfiles/sidecar.dockerfile",
    "../../target/release/test-sidecar",
    "sidecar:network_tests",
)
.await;

// build the swarm image
let test_router_jsonnet = std::path::PathBuf::from("../../swarm/configs/test_router.jsonnet");
let test_peer_jsonnet = std::path::PathBuf::from("../../swarm/configs/test_peer.jsonnet");
SwarmImage::build(
    &docker,
    "assets/dockerfiles/swarm.dockerfile",
    "../../target/release/swarm",
    "swarm:network_tests",
    &[
        (test_router_jsonnet.as_path(), "test_router.jsonnet"),
        (test_peer_jsonnet.as_path(), "test_peer.jsonnet"),
    ],
)
.await;
```

The example mentions a sidecar image. The sidecar solution solves connection problems under rootless
docker installations (and likely also podman for that reason) as it is not possible to directly 
connect to IP addresses of containers from the host system in that environment. For that reason a
sidecar service is deployed to a specifc network. The sidecar for now offers an incomplete HTTP 
interface of functions that need to connect to services running inside the container, to achieve 
that the sidecar usually exposes a port to the host. For example the sidecar can make connection 
with the `sorg-client` crate and query information from inside the network.

After the general setup above a test can be fairly simple:
```rust
// compose up
let compose = ComposeProject::up(
    "my_compose.yaml",
    "my_project",
)
.await;

// test logic that interacts with sidecar or runs commands inside the compose containers

// compose down
compose.down().await;
```


# Swarm Tests 

---
Prebuild binaries of `swarm`, `myrmic` are required. 
---

Swarm tests are running typically running locally. The normal test setup up looks like this:
```rust
let myrmic = Myrmic::local();
let swarm = Swarm::local();

let process = swarm
    .spawn(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/path/to/swarm/config.jsonnet"
    ))
    .await;
```

As a second step you would usually build one or more cells and register the artifact(s):
```rust
let cell_artifact = myrmic
    .build(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/path/to/test/cell/Cargo.toml"
    ))
    .await;
cell_artifact.register(process.session()).await;
```

For interactions you can then get a wrapper around `sorg-client` to load a cell:
```rust
let mut sorg = SorgHandle::connect(process.session().clone()).await;
sorg.load_cell("cell.wasm", "cell.SRI").await;
```

And from now on you can send command or events via the `SorgHandle`.
