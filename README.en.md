# Torquetum API

[繁體中文](README.md) · English

The local HTTP API provides Project pairing, status queries, and Asterism geometry exchange. This repository contains the OpenAPI V0.0 contract, the Geometry Collection JSON Schema, and the Rust and TypeScript code that Torquetum uses to serve the API. External applications connect to a running Torquetum Project over HTTP. [OpenAPI contract](contracts/openapi.yaml) · [Geometry Collection Schema](contracts/geometry-collection.schema.json) · [Alidade for Grasshopper](https://github.com/Avery320/alidade)

Use your own geometry source and HTTP client. Convert the source geometry to a Geometry Collection and use the pairing grant to send it to the chosen Asterism Input ID. Torquetum validates the collection, updates the Input, evaluates downstream nodes, and makes the Output available to read. This repository provides the contracts, JSON samples, and a runnable Python example. [Geometry format](#geometry)

## API overview

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/v0.0/pair` | Redeem a one-use pairing invitation for a Project `accessToken`. |
| `GET` | `/v0.0/project` | Get the Project identity, read and write permissions, exchange readiness, and transfer limits. |
| `GET` | `/v0.0/asterism/ports` | List addressable Geometry Input and Geometry Output IDs and the ETag of each Input. |
| `PUT` | `/v0.0/asterism/inputs/{id}` | Replace the full geometry tree at an Input. The response to an accepted write includes the receipt location. |
| `GET` | `/v0.0/requests/{requestId}` | Use the original write grant to check whether the receipt is `pending`, `committed`, or `rejected`. |
| `GET` | `/v0.0/asterism/outputs/{id}` | Read the completed geometry tree at an Output. Include `X-Tor-Request-Id` to read the result for a specific write. |

## Quick start

Run Torquetum Desktop or local Web served by `torquetum-server` on the same computer as the client. Use a version that supports Torquetum API V0.0.

1. Open a standalone test Project in Studio. In Asterism, wire a **Geometry Input** directly to a **Geometry Output**. Enable both and set both **ID** fields to `fixture`.
2. In **Project Settings → External Connections** for that Project, select **Copy** next to **Pairing code**.
3. Clone the API repository, run the Python example and paste the code at its prompt. [Python example](examples/python/asterism-exchange-smoke.py)

   ```sh
   git clone https://github.com/Avery320/torquetum-api.git
   cd torquetum-api
   python3 examples/python/asterism-exchange-smoke.py fixture fixture
   ```

The script uses the Python 3 standard library. It updates the test Input twice with different coordinates for the same point, reads back the exact revisions, and checks retry and stale-version handling. A successful run prints `Verified two revisions of the same Asterism input, exact output reads and idempotent retries.`

## Make HTTP requests

Use the Quick start test nodes. Run these commands in order from the API repository root in the same shell. They use `curl` and Python 3.

Copy a fresh pairing code from the same Project. Decode the part after `TORAPI.` from unpadded base64url to UTF-8 JSON, then read `endpoint` and `invite`. Send every request to the `http://127.0.0.1:<port>` in `endpoint`. Each code can be used once. Copy a fresh code if the invitation expires or the grant becomes unavailable. [Pairing code decoder](examples/python/asterism-exchange-smoke.py)

1. **Get a grant.** Replace the example URL and `<invite>` with the decoded values. Copy the `accessToken` from the successful response. [Pairing request and response sample](fixtures/connection.json)

   ```sh
   tor_endpoint='http://127.0.0.1:44555'
   curl --noproxy '*' -sS "$tor_endpoint/v0.0/pair" \
     -H 'Content-Type: application/json' \
     --data '{"invite":"<invite>","clientName":"Geometry example"}'
   ```

2. **Check Project status and the Input revision.** Set the actual `accessToken`. Check `canWrite` before writing, and read supported geometry types and transfer limits from `kinds` and `limits`. In the node list, find the item with `direction` equal to `input` and `id` equal to `fixture`. Use its decoded `etag` string, including the surrounding double quotes.

   ```sh
   tor_token='<accessToken>'
   curl --noproxy '*' -sS "$tor_endpoint/v0.0/project" \
     -H "Authorization: Bearer $tor_token"
   curl --noproxy '*' -sS "$tor_endpoint/v0.0/asterism/ports" \
     -H "Authorization: Bearer $tor_token"
   ```

3. **Send mixed geometry.** Set the ETag you just looked up. Use a new UUID for each new write. The command sends the JSON sample directly to the `fixture` Input and replaces its full tree. The sample contains a point, a line, a polyline, a plane, a box, and a mesh. [Basic collection sample](fixtures/geometry-collection-basic.json)

   ```sh
   tor_etag='"input-v1"'
   tor_request_id=$(python3 -c 'from uuid import uuid4; print(uuid4())')
   curl --noproxy '*' -sS -i -X PUT "$tor_endpoint/v0.0/asterism/inputs/fixture" \
     -H "Authorization: Bearer $tor_token" \
     -H 'Content-Type: application/json' \
     -H "If-Match: $tor_etag" \
     -H "X-Tor-Request-Id: $tor_request_id" \
     --data-binary @fixtures/geometry-collection-basic.json
   ```

4. **Check the write.** `202 Accepted` means the write was accepted for processing. Set `tor_receipt` to the full relative path from the response's `Location` header and check the receipt using the original grant. For `pending`, wait the number of seconds in `Retry-After` and check again. A `committed` receipt confirms that the Input was updated, and `result.etag` gives its new revision. For `rejected`, inspect `result.error`. Geometry validation errors can include `path`, `itemIndex`, and `field`.

   ```sh
   tor_receipt='/v0.0/requests/<requestId>'
   curl --noproxy '*' -sS -i "$tor_endpoint$tor_receipt" \
     -H "Authorization: Bearer $tor_token"
   ```

5. **Read the result of this write.** Read the Output that depends on that Input and send the same `X-Tor-Request-Id`. Retry the read if `409 OUTPUT_NOT_READY` indicates that Asterism is still evaluating. `INPUT_SUPERSEDED` means a newer write replaced this Input revision. `OUTPUT_UNAVAILABLE` means there is no usable geometry. A successful response contains a Geometry Collection with an `ETag` matching the receipt's `result.etag`. In the sample, branch `[0]` contains the point `[1, 2, 3]` metres, and branch `[3, 0, 7]` contains a triangle mesh.

   ```sh
   curl --noproxy '*' -sS -i "$tor_endpoint/v0.0/asterism/outputs/fixture" \
     -H "Authorization: Bearer $tor_token" \
     -H "X-Tor-Request-Id: $tor_request_id"
   ```

Replace the sample JSON with your own Collection and update the URLs to match the connected nodes. Use each Input or Output node's **ID**. IDs accept ASCII letters, digits, hyphens, and underscores. They are case-sensitive and unique within each direction. To read the latest completed Output, omit `X-Tor-Request-Id`.

If a write's outcome is unknown, keep the original grant, UUID, URL, headers, and JSON bytes, and rerun just the `curl` command from step 3. For `412 STALE_REVISION`, fetch the current Input ETag and decide whether to submit a new write with a new UUID.

Native clients connect directly with redirects, proxies and cookies disabled. The host checks the peer and `Host` and rejects browser `Origin` on native routes. Pairing works from a draft. Open the same Project in Studio before exchanging geometry. A standalone Project supports reads and writes. A LAN Project supports reads after synchronization. [Full HTTP contract](contracts/openapi.yaml)

Each write replaces the full Input tree and queues Asterism evaluation. An invalid write retains the previous snapshot. Clearing or changing an Input ID clears its data. Disabling suspends exchange and changes the ETag. Re-enabling the same Input retains its snapshot. Closing the Project releases the grant and in-memory exchange data. Use **Bake** or **Bake Geometry Tree** to create Station objects.

## Geometry

Input writes and Output reads use the same Geometry Collection format. The top-level fields are `schemaVersion: "0.0"`, `units: "m"`, `space: "project"`, and `branches`. Coordinates use the right-handed, Z-up Project world frame. Coordinates and lengths are in metres and must be finite.

Each branch has a `path` and `items`. A path is a nonempty array of nonnegative integers, such as `[0, 4]`, and is unique within the Collection. The items can mix geometry types. On readback, branches are sorted by path, and the order of items, `null` slots, and empty branches are preserved. An Input write requires at least one geometry item.

| Geometry | JSON `kind` | Fields and constraints |
| --- | --- | --- |
| Point | `point` | `position` is an XYZ coordinate `[x, y, z]`. |
| Line (Curve) | `line` | `start` and `end` are distinct XYZ coordinates. Optional `domain: [t0, t1]` is strictly increasing and defaults to `[0, 1]`. |
| Polyline (Curve) | `polyline` | `points` contains ordered XYZ coordinates and `closed` is a boolean. Consecutive points must differ. An open polyline needs at least two points, and a closed polyline needs at least three distinct points with the repeated closing point omitted. Optional `domain: [t0, t1]` is strictly increasing and defaults to 0 through the number of segments. |
| Plane | `plane` | `origin` is an XYZ position, `normal` is the plane normal, and `direction` is the reference X direction. Both vectors must be nonzero and nonparallel. |
| Box | `box` | `plane` contains `origin`, `normal`, and `direction`. Supply a strictly increasing `[min, max]` range for each of `x`, `y`, and `z` along the plane's local axes. |
| Mesh | `triangle-mesh` | `positions` contains at least three XYZ vertices and `triangles` contains at least one face. Each face has three valid zero-based vertex indices and a positive area. |
| Surface and BRep | `brep-step` | Set `shapeType` to `face` for a single face, `shell` for a shell, or `solid` for a solid. Supply the complete STEP Part 21 text for one connected shape in `step`. |

- Curve exchange supports lines and polylines. Standalone circles, arcs, and general NURBS curves have no exchange type in V0.0.
- Meshes carry vertices and triangle faces. Triangulate quads or polygons in the source before writing the indices.
- For BRep, the receiver checks that the shape matches `shapeType` and uses the units declared in the STEP file. Torquetum keeps the exact CAD shape and prepares a mesh for display.

Use the samples below as a starting point for your own Collection. For Rhino geometry conversion and Grasshopper instructions, see the Alidade guide. [Basic collection](fixtures/geometry-collection-basic.json) · [Mixed BRep collection](fixtures/geometry-collection.json) · [Geometry Collection Schema](contracts/geometry-collection.schema.json) · [Alidade guide](https://github.com/Avery320/alidade#readme)

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

If you have access to the Torquetum source repository, follow its development setup to initialize the submodule and install dependencies. Then run `pnpm --filter @torquetum/studio build` and `cargo run -p torquetum-server` to start local Web. [Torquetum development setup](https://github.com/Avery320/torquetum#開發環境)

## License

Distributed under the [MIT License](LICENSE).
