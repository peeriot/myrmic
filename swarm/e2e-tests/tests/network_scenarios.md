# Zenoh P2P Layer — Detailed Test Scenarios

> Scope: zenoh core.
> Tools:
> - network-test-suite.
> - test control plugin.
> Nodes:
> - **P** = peer
> - **R** = router


> `Baseline policy` in these scenarios refers to a **network policy configuration in the Network Test Suite** that represents the normal, fully connected state of the network. In other words, **All nodes can communicate without restrictions** no partitions, no latency shaping.
> In the context of these test scenarios, **partition** refers to a network segmentation applied by the **network test suite** that **blocks direct communication** between specified nodes.

## A1 — Discovery

**Network Structure**
```
Network
 P1 <----> P2
```
- Both peers on the same network.
- All traffic allowed between P1 and P2.
- No routers.

**Goal**
Test multicast discovery between peers.

**Input**
- Policy: `baseline`.
- Config: `mode: peer`, multicast scouting enabled.
- Actions:
  1. On both peers, call introspection to get the node status.

**Expected Output**
- Each peer's node status contains the other’s ZID.

> We test also the same same setup without discovery, In this case the peers should not be connected to each other.

## A2 — Gossip Discovery Across networks via Router

**Network Structure**
```
Network-a              Network-b
       Pa <--> R <--> Pb
```
- Pa connected only to R.
- Pb connected only to R.

**Goal**
Verify discovery via router gossip and data flow.

**Input**
- Policy: direct connection between Pa and Pb, and allow connection between Pa/Pb and R1.
- Router mode on R1.
- Actions:
  1. On all nodes, call introspection to get the node status. introspection method.
  2. Sa subscribes to `tests/a2`.
  3. Sb publishes 5 messages.


**Expected Output**
- Pa and Pb don't know each other directly.
- Both Pa and Pb ZIDs are listed in the node status of R.
- All 5 messages received by Pa.

## A3 — Partition and Heal

**Network Structure**
```
Network
 P1 <----> P2
```

**Goal**
Ensure peers reconnect after partition.

**Input**
- Policy: baseline → partition → baseline.
- Actions:
  1. Check nodes status before, during, after partition via the introspection.

**Expected Output**
- Pre-partition, P1 and P2 are connected to each other.
- P1 and P2 lose connection to each other during partition.
- P1 and P2 reconnects within a specific time after heal.

## A4 — Dynamic Publisher Discovery
**Network Structure**
```
Network
 P1 <----> P2 <----> P3
```
**Goal**
Verify that subscribers automatically receive updates from publishers that join after the subscription is created.

**Input**
- Policy: baseline.
- Actions:
  1. Start P1 as subscriber to `tests/a4`.
  2. Start S2 as publisher to `tests/a4` and publish 5 messages.
  3. After initial reception, start P3 as new publisher to the same key.
  4. P3 publishes 5 messages.

**Expected Output**
- P1 receives messages from P2.
- Upon P3’s new publisher creation, P1 automatically receives messages from S3 without restarting subscription.
- In total P1 receives 10 messages.

## B1 — Single-Level Wildcard
```
Network
 P1 <----> P2
```
Goal: Confirm `*` matches exactly one segment.

Input:
- P1 subscribes to `foo/*/bar`.
- P2 publishes `foo/x/bar` and `foo/x/y/bar`.

Expected:
- Only `foo/x/bar` received.

## B2 — Multi-Level Wildcard
```
Network
 P1 <----> P2
```
Goal: Confirm `**` matches multiple segments.

Input:
- P1 subscribes to `foo/**`.
- P2 publishes `foo/a`, `foo/a/b`, `bar/foo`.

Expected:
- Receives first two, not `bar/foo`.

## C1 — Queryable Present
```
Network
 P1 <----> P2
```
Goal: Successful query to matching peer.

Input:
- P2 registers a queryable at `tests/c1`.
- P1 queries `tests/c1` using get request.

Expected:
- Query succeeds.

## C2 — Queryable Absent
```
Network
 P1 <----> P2
```
Goal: Timeout when no queryable exists.

Input:
- S1 queries `tests/c2`.

Expected:
- Nno data received.

---

## C3 — Query Partition and Heal
```
Network
 P1 <----> P2
```
Goal: Query fails during partition, succeeds after heal.

Input:
- Policy change partition --> baseline.
- Query before, during, after partition.

Expected:
- No data is received during the partition.
- Success within a certain time after heal.
