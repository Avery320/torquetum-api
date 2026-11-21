# Torquetum API

[繁體中文](README.md) · English

Local HTTP API for Torquetum Project access and Asterism geometry exchange. This repository contains the [OpenAPI V0.0 contract](contracts/openapi.yaml), [Geometry Collection schema](contracts/geometry-collection.schema.json) and the Rust and TypeScript code used by Torquetum to serve the API. Native clients connect to a running Torquetum Project over HTTP. [Alidade](https://github.com/Avery320/alidade) is the Grasshopper connector.

## Quick start

Run Torquetum Desktop or a local `torquetum-server` with V0.0 support on the same computer as the client. With access to the Torquetum source checkout, install its dependencies, build Studio with `pnpm --filter @torquetum/studio build`, run `cargo run -p torquetum-server` and open the URL printed by the server. The first API import comes from Torquetum commit `942ddd33307966e8b6cbe1b2ad8ed5856c454feb`.

1. Open a standalone Project in Studio. In Asterism, wire a **Geometry Input** directly to a **Geometry Output**. Enable both and set both **ID** fields to `fixture`.
2. Copy the pairing code from **Project Settings → External Connections** for that Project.
3. Run the [Python example](examples/python/asterism-exchange-smoke.py) from the cloned API repository and paste the code at its prompt.

   ```sh
   python3 examples/python/asterism-exchange-smoke.py fixture fixture
   ```

The script uses the Python standard library. It replaces the test Input tree twice, checks the write receipts, reads back the exact revisions and verifies retry and stale-version handling. Use a dedicated test Input for this example.

## Connect a client

The pairing code starts with `TORAPI.` and contains unpadded base64url UTF-8 JSON with `endpoint` and `invite`. Send a JSON body with `invite` and `clientName` to `POST /v0.0/pair` at that endpoint and use the returned `accessToken` as a Bearer token. `GET /v0.0/project` reports the current Project, access, readiness and transfer limits. An invitation is usable once. Copy a fresh code when an invitation expires or a grant becomes unavailable.

Connect directly to `http://127.0.0.1:<port>` with redirects, proxies and cookies disabled. The host checks the peer and `Host` and rejects browser `Origin` on native routes. Pairing works from a draft. Open the same Project in Studio before exchanging geometry. A standalone Project supports reads and writes. A LAN Project supports reads after synchronization.

## Send and read geometry

1. Set an **ID** on an enabled Asterism **Geometry Input** and **Geometry Output**. Use letters, numbers, hyphens or underscores. IDs are case-sensitive and unique within each direction.
2. Obtain the Input's strong ETag from `GET /v0.0/asterism/ports`. Send a complete tree to `PUT /v0.0/asterism/inputs/{id}` with `If-Match` and a new UUID in `X-Tor-Request-Id`.
3. Follow the `202` response's relative `Location` with `GET /v0.0/requests/{requestId}` on the same host and with the same grant. The receipt is `pending`, `committed` or `rejected`. The committed result contains the Input `id` and its new `etag`. Retry uncertain writes with the same UUID, path, headers and body bytes.
4. Read `GET /v0.0/asterism/outputs/{id}`. To read the result for a specific Input write, include its UUID in `X-Tor-Request-Id`. The successful response's ETag identifies the Input revision. Retry briefly for `OUTPUT_NOT_READY`. `OUTPUT_UNAVAILABLE` means the result has no usable geometry. A superseded Input revision returns `INPUT_SUPERSEDED`.

Each write replaces the full Input tree and queues Asterism evaluation. An invalid write retains the previous snapshot. Clearing or changing an Input ID clears its data. Disabling suspends exchange and changes the ETag. Re-enabling the same Input retains its snapshot. Closing the Project releases the grant and in-memory exchange data. Use **Bake** or **Bake Geometry Tree** to create Station objects.

## Geometry

The tree uses `schemaVersion: "0.0"`, Project world coordinates and metres. Branch paths, item order, null slots and empty branches are preserved. The collection can mix points, lines, polylines, planes, boxes, triangle meshes and exact STEP faces, shells or solids.

Each STEP item represents one connected shape. The receiver checks its declared type and uses the declared STEP units. Torquetum preserves the exact source geometry and prepares a tessellation for viewing. For unsupported items or invalid dimensions, inspect the branch and item location in the error. Transfer limits are advertised by the paired Project.

Use the synthetic [connection example](fixtures/connection.json), [basic collection](fixtures/geometry-collection-basic.json) and [mixed STEP collection](fixtures/geometry-collection.json) to check client payloads. For Rhino geometry conversion and Grasshopper instructions, see the [Alidade guide](https://github.com/Avery320/alidade#readme).

## Structure

```text
contracts/      OpenAPI and Geometry Collection JSON Schema
fixtures/       Sample connection and geometry payloads
examples/       Python end-to-end exchange example
rust/           HTTP host, internal bridge transport and Rust tests
src/            TypeScript bridge, pairing and receipt handling
tests/          TypeScript session tests and shared bridge fixture
```

## Development

Use Node.js 22.12 or later, pnpm 10 and Rust 1.91.1 or later. Run these checks from the repository root.

```sh
pnpm install --frozen-lockfile
pnpm typecheck
pnpm test
cargo test --manifest-path rust/Cargo.toml --locked
```

The unit tests exercise the host and session with fixtures. Run the Quick start against a Torquetum Project to verify the full HTTP exchange.

## License

Distributed under the [MIT License](LICENSE).
