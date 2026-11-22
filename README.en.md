# Torquetum API

[繁體中文](README.md) · English

The local HTTP API provides Project pairing, status queries, and Asterism geometry exchange. This repository contains the OpenAPI V0.0 contract, the Geometry Collection JSON Schema, and the Rust and TypeScript code that Torquetum uses to serve the API. External applications connect to a running Torquetum Project over HTTP. [OpenAPI contract](contracts/openapi.yaml) · [Geometry Collection Schema](contracts/geometry-collection.schema.json) · [Alidade for Grasshopper](https://github.com/Avery320/alidade)

Use your own geometry source and HTTP client. Convert the source geometry to a Geometry Collection and use the pairing grant to send it to the chosen Asterism Input ID. Torquetum validates the collection, updates the Input, evaluates downstream nodes, and makes the Output available to read. This repository provides the contracts, JSON samples, and a runnable Python example. [Geometry format](#geometry)

## API overview

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/v0.0/pair` | Exchange a one-use invitation or remembered credential for a Project `accessToken`. |
| `GET` | `/v0.0/project` | Get the Project identity, read and write permissions, exchange readiness, and transfer limits. |
| `GET` | `/v0.0/asterism/ports` | List addressable Geometry Input and Geometry Output IDs and the ETag of each Input. |
| `PUT` | `/v0.0/asterism/inputs/{id}` | Stage a full geometry tree for the Input to apply when the user presses Read. An accepted write includes the receipt location. |
| `GET` | `/v0.0/requests/{requestId}` | Use the original write grant to check whether the receipt is `pending`, `committed`, or `rejected`. |
| `GET` | `/v0.0/asterism/outputs/{id}` | Read the geometry tree most recently published using Send on the Output. |

## Quick start

Run Torquetum Desktop or local Web served by `torquetum-server` on the same computer as the client, and open the Project in Studio. Use a version that supports Torquetum API V0.0. Sending and reading share one pairing grant and can be used independently.

- **To send only**, add a **Geometry Input** from **Exchange** in Asterism. [Send geometry](#send-geometry)
- **To read only**, add a **Geometry Output** from **Exchange** and connect the geometry to it. [Read geometry](#read-geometry)

Input and Output IDs can be set independently. They accept ASCII letters, digits, hyphens, and underscores and are case-sensitive and unique within each direction.

The commands below use `curl`, Python 3, and samples from this repository. Run them in the same shell.

```sh
git clone https://github.com/Avery320/torquetum-api.git
cd torquetum-api
```

### Pair the Project

In **Project Settings → External Connections**, select **Copy** next to **Pairing code**. Decode the part after `TORAPI.` from unpadded base64url to UTF-8 JSON, then read `endpoint` and `invite`. Send every request to the `http://127.0.0.1:<port>` in `endpoint`. Each code can be used once. Copy a fresh code if the invitation expires or the grant becomes unavailable. [Pairing code decoder](examples/python/asterism-exchange-smoke.py)

Replace the example URL and `<invite>` with the decoded values, then take the `accessToken` from the successful response. [Pairing request and response sample](fixtures/connection.json)

```sh
tor_endpoint='http://127.0.0.1:44555'
curl --noproxy '*' -sS "$tor_endpoint/v0.0/pair" \
  -H 'Content-Type: application/json' \
  --data '{"credential":"<invite>","clientName":"Geometry example"}'
```

Set `tor_token` to the returned `accessToken` and use it for either direction. Query `GET /v0.0/project` to check `canRead`, `canWrite`, the supported geometry `kinds`, and transfer `limits`.

```sh
tor_token='<accessToken>'
curl --noproxy '*' -sS "$tor_endpoint/v0.0/project" \
  -H "Authorization: Bearer $tor_token"
```

Native clients connect directly with redirects, proxies and cookies disabled. The host checks the peer and `Host` and rejects browser `Origin` on native routes. Pairing works from a draft. Open the same Project in Studio before exchanging geometry. A standalone Project supports reads and writes. A LAN Project supports reads after synchronization. [Full HTTP contract](contracts/openapi.yaml)

### Remember a client

Pairing codes are single-use and expire after five minutes. An `accessToken` is 64 characters and lasts eight hours. Closing the Project ends its current access sessions.

To remember a client, generate a random 32-byte secret and include its SHA-256 lowercase hex digest as `remember` in the first pairing request. Keep the secret, returned `locator`, and `projectId` in local storage scoped to the OS user. The authorization belongs to the original Studio origin and Project.

After reopening the Project in that Studio, read the live instance record in `~/.torquetum/connections/projects/<locator>/`. Each JSON file holds `port`, `instanceId`, and `processId`. Encode the running instance's 16 UUID bytes in standard byte order followed by the original 32 secret bytes as unpadded base64url. Use that value as `credential` on the same `/v0.0/pair` route and check the returned `projectId` and `locator`. **Revoke** in **External Connections** also revokes the ability to reconnect.

## Send geometry

In a standalone Project, add a **Geometry Input** in Asterism and set its **ID** to `source`. Send a Collection to that ID. After the write commits, press **Read** to apply it to the graph.

1. **Get the Input revision.** Find the item with `direction` equal to `input` and `id` equal to `source` in the node list. Use its decoded `etag` string, including the surrounding double quotes.

   ```sh
   curl --noproxy '*' -sS "$tor_endpoint/v0.0/asterism/ports" \
     -H "Authorization: Bearer $tor_token"
   ```

2. **Send mixed geometry.** Set the ETag you just looked up. Use a new UUID for each new write. The command sends the JSON sample directly to the `source` Input and replaces its full tree. The sample contains a point, a line, a polyline, a plane, a box, and a mesh. [Basic collection sample](fixtures/geometry-collection-basic.json)

   ```sh
   tor_etag='"input-v1"'
   tor_request_id=$(python3 -c 'from uuid import uuid4; print(uuid4())')
   curl --noproxy '*' -sS -i -X PUT "$tor_endpoint/v0.0/asterism/inputs/source" \
     -H "Authorization: Bearer $tor_token" \
     -H 'Content-Type: application/json' \
     -H "If-Match: $tor_etag" \
     -H "X-Tor-Request-Id: $tor_request_id" \
     --data-binary @fixtures/geometry-collection-basic.json
   ```

3. **Check the write.** `202 Accepted` means the write was accepted for processing. Set `tor_receipt` to the full relative path from the response's `Location` header and check the receipt using the original grant. For `pending`, wait the number of seconds in `Retry-After` and check again. A `committed` receipt confirms that the tree is ready for the Input to read, and `result.etag` gives its new revision. For `rejected`, inspect `result.error`. Geometry validation errors can include `path`, `itemIndex`, and `field`.

   ```sh
   tor_receipt='/v0.0/requests/<requestId>'
   curl --noproxy '*' -sS -i "$tor_endpoint$tor_receipt" \
     -H "Authorization: Bearer $tor_token"
   ```

Replace the sample JSON with your own Collection and put the target Input ID in the request path. If a write's outcome is unknown, keep the original grant, UUID, URL, headers, and JSON bytes, then rerun just the `PUT` command. For `412 STALE_REVISION`, fetch the current Input ETag and decide whether to submit a new write with a new UUID.

The next write to the same ID replaces the staged tree. The graph keeps using the last tree it read until the next **Read**. Changing a node ID, deleting the node, or closing the Project clears its exchange data. Use **Bake** or **Bake Geometry Tree** to create Station objects.

## Read geometry

Connect Asterism geometry to a **Geometry Output** and set its **ID** to `result`. Press **Send** to publish a snapshot, then read it by Output ID.

```sh
curl --noproxy '*' -sS -i "$tor_endpoint/v0.0/asterism/outputs/result" \
  -H "Authorization: Bearer $tor_token"
```

`409 OUTPUT_UNAVAILABLE` means this Output has no published snapshot. After changing the upstream geometry, press **Send** again to publish the new snapshot.

## Verify a round trip

In a standalone test Project, connect the **Geometry Input** with ID `source` to the **Geometry Output** with ID `result`. Run this Python example and paste a fresh pairing code. At each prompt, press **Read** and then **Send** in Asterism, then press Enter to read the result. The script verifies two manual exchanges using the same IDs, write retries, and stale-version handling. [Python example](examples/python/asterism-exchange-smoke.py)

```sh
python3 examples/python/asterism-exchange-smoke.py source result
```

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

The unit tests exercise the host and session with fixtures. Use a running Torquetum Project to verify the full exchange. The first API import comes from Torquetum commit `942ddd33307966e8b6cbe1b2ad8ed5856c454feb`. [Round-trip verification](#verify-a-round-trip)

If you have access to the Torquetum source repository, follow its development setup to initialize the submodule and install dependencies. Then run `pnpm --filter @torquetum/studio build` and `cargo run -p torquetum-server` to start local Web. [Torquetum development setup](https://github.com/Avery320/torquetum#開發環境)

## License

Distributed under the [MIT License](LICENSE).
