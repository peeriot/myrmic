# Security

This page covers the security mechanisms the developer preview implements today.

## Transport Authentication and Encryption

Myrmic Nodes can authenticate each other and protect the traffic between them with mutual TLS. It is configured on the Zenoh transport link, where `enable_mtls` turns on certificate verification in both directions and `verify_name_on_connect` requires the name a Node is reached by to appear in the Subject Alternative Name of the certificate it presents, with endpoints declared under the `tls/` scheme. Each Node presents its own certificate chain for both listening and connecting and validates its peer against a configured root certificate, so reaching the transport requires a certificate issued under that anchor. The Zenoh session runs inside that link, so traffic between two directly connected Nodes is encrypted and integrity-protected, and because Zenoh relays hop by hop a forwarding Node terminates the link and handles what it passes on in the clear. 

Mutual TLS is off in the default configuration and is turned on per deployment. Issuing the certificates is the deployment's responsibility, because the preview ships no CA service, and `swarm/tutorials/mTLS/` brings up a complete multi-node environment with a locally created CA hierarchy that stands in for one.

## Cell Isolation

Every Cell runs as a WebAssembly module inside a runtime that mediates its access to the Node. On OS-based Nodes that runtime is Wasmtime, and on supported MCUs it is WAMR, compiled ahead of time and executed in place from flash. In both cases the module reaches the host only through the host-call families its runner links into it, which include Cell commands and events, timers, the data layer, and the Signal Layer taps and outlets. Neither target links WASI, so a Cell has no ambient access to the filesystem, to sockets, to processes, or to the host clock, and it addresses its own linear memory alone. On OS-based Nodes the runtime meters guest instruction execution with Wasmtime fuel, so the computation a Cell performs is bounded by the fuel its runner grants it.

## Coming

The security work still ahead, including an authenticated fabric with a full key lifecycle so that Nodes and Cells carry cryptographic proof of where they come from, is the **Secure Swarm** stage of the [Roadmap](./09_roadmap.md).
