# Torquetum API

[繁體中文](README.md) · English

Local HTTP API for Torquetum Project access and Asterism geometry exchange. This repository contains the [OpenAPI V0.0 contract](contracts/openapi.yaml), [Geometry Collection schema](contracts/geometry-collection.schema.json) and the Rust and TypeScript code used by Torquetum to serve the API. Native clients connect to a running Torquetum Project over HTTP. [Alidade](https://github.com/Avery320/alidade) is the Grasshopper connector.

## Quick start

Run a V0.0-capable Torquetum Desktop app or local `torquetum-server` on the same computer as the client. If you have access to the Torquetum source repository, follow its development setup to initialize the submodule and install dependencies. Build Studio with `pnpm --filter @torquetum/studio build`, run `cargo run -p torquetum-server` and open the URL printed by the server. [Torquetum development setup](https://github.com/Avery320/torquetum#開發環境)

1. Open a standalone test Project in Studio. In Asterism, wire a **Geometry Input** directly to a **Geometry Output**. Enable both and set both **ID** fields to `fixture`.
2. Copy the pairing code from **Project Settings → External Connections** for that Project.
3. Clone the API repository, run the Python example and paste the code at its prompt. [Python example](examples/python/asterism-exchange-smoke.py)

   ```sh
   git clone https://github.com/Avery320/torquetum-api.git
   cd torquetum-api
   python3 examples/python/asterism-exchange-smoke.py fixture fixture
   ```

The script uses the Python 3 standard library. It replaces the test Input tree twice, reads back the exact revisions and checks retry and stale-version handling. A successful run prints `Verified two revisions of the same Asterism input, exact output reads and idempotent retries.`

## Make HTTP requests

Use the Quick start test nodes and copy a fresh pairing code from the same Project. Decode the part after `TORAPI.` as unpadded base64url UTF-8 JSON to obtain `endpoint` and `invite`. Send every request to the `http://127.0.0.1:<port>` in `endpoint`. An invitation is usable once. Copy a fresh code when an invitation expires or a grant becomes unavailable.

1. **Get a grant.** Send `{"invite":"<invite>","clientName":"Point example"}` to `POST /v0.0/pair` with `Content-Type: application/json`. Use the returned `accessToken` in `Authorization: Bearer <accessToken>` for every subsequent request. Use `GET /v0.0/project` to check Project status, `canWrite` and transfer limits. [Example pairing request and response](fixtures/connection.json)
2. **Get the Input revision.** Call `GET /v0.0/asterism/ports` and find the item with `direction` equal to `input` and `id` equal to `fixture`. Use its decoded `etag` string as `If-Match`, including the double quotes, as in `"input-v1"`.
3. **Send a point.** The following request replaces the `fixture` Input tree. Substitute the actual port, token and ETag and create a new UUID for each new write. The point is at `[1, 0, 0]` in Project world coordinates, measured in metres.

   ```http
   PUT /v0.0/asterism/inputs/fixture HTTP/1.1
   Host: 127.0.0.1:<port>
   Authorization: Bearer <accessToken>
   Content-Type: application/json
   If-Match: "input-v1"
   X-Tor-Request-Id: bd9499c5-0552-4309-87bf-cdbe9217d48e

   {
     "schemaVersion": "0.0",
     "units": "m",
     "space": "project",
     "branches": [{"path": [0], "items": [{"kind": "point", "position": [1, 0, 0]}]}]
   }
   ```

4. **Check the write.** `202 Accepted` confirms admission. Use the same grant to send `GET` to the relative `Location` on the same host.

   ```http
   HTTP/1.1 202 Accepted
   Content-Type: application/json
   Location: /v0.0/requests/bd9499c5-0552-4309-87bf-cdbe9217d48e
   Retry-After: 1

   {"requestId": "bd9499c5-0552-4309-87bf-cdbe9217d48e"}
   ```

   Follow `Retry-After` while the receipt is `pending`. Read `result.error` if it is `rejected`. A `committed` receipt confirms the Input was updated, and `result.etag` gives its new revision.

5. **Read this write's result.** Call `GET /v0.0/asterism/outputs/fixture` with the same `X-Tor-Request-Id`. A `409 OUTPUT_NOT_READY` means Asterism is still evaluating. Retry the read after a short wait. A successful read returns the tree containing the same point, and its `ETag` matches the receipt's `result.etag`. A newer revision replacing the Input returns `INPUT_SUPERSEDED`. `OUTPUT_UNAVAILABLE` means the result has no usable geometry.

Retry an uncertain write with the same UUID, path, headers and body bytes. Input and Output IDs can be set independently. Use letters, numbers, hyphens or underscores. IDs are case-sensitive and unique within each direction.

Native clients connect directly with redirects, proxies and cookies disabled. The host checks the peer and `Host` and rejects browser `Origin` on native routes. Pairing works from a draft. Open the same Project in Studio before exchanging geometry. A standalone Project supports reads and writes. A LAN Project supports reads after synchronization. [Full HTTP contract](contracts/openapi.yaml)

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

The unit tests exercise the host and session with fixtures. Run the Quick start against a Torquetum Project to verify the full HTTP exchange. The first API import comes from Torquetum commit `942ddd33307966e8b6cbe1b2ad8ed5856c454feb`.

## License

Distributed under the [MIT License](LICENSE).
