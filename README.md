# earthnet-node

Node for the [EarthNet](https://github.com/devjamez/earthnet-protocol) earthquake
early-warning network. Ingests signed `Observation`s from country adapters and
sensors, fuses them + reaches consensus, and emits signed `ConfirmedEvent`s that
trigger client alarms.

## Trust model (DESIGN §5)

- **OFFICIAL** source (e.g. a CSN/Chile adapter) with a P-wave pick → fires on its own.
- **PHONE** source → requires consensus of ≥ N correlated picks.

## Ingest API

Adapters POST a single signed Observation (raw protobuf bytes) to the node:

```
POST /observations    body = Observation protobuf
  202 Accepted     verified + ingested
  400 Bad Request  undecodable / bad fields
  401 Unauthorized signature failed
GET  /health → "ok"
```

Phone consensus is **spatial + temporal**: ≥ N picks within `radius_km` and
`window_s` of each other (correlated by decoded geohash + capture time).

## Run

```sh
cargo run
# env: EARTHNET_NODE_ADDR (127.0.0.1:8080), EARTHNET_CONSENSUS_N (3),
#      EARTHNET_CONSENSUS_RADIUS_KM (100), EARTHNET_CONSENSUS_WINDOW_S (30),
#      EARTHNET_NODE_KEY_FILE (node_key.hex), EARTHNET_NODE_KEY (hex seed, overrides file)
```

## Status

🟡 v0.2 — HTTP ingest + signature verification + spatial/temporal consensus +
persisted identity. NOT yet modeled: magnitude/epicenter estimation, event
revision, relay fan-out wiring, Sybil/reputation.

## License

AGPL-3.0-or-later.
