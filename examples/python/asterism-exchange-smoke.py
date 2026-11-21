#!/usr/bin/env python3
"""Verify the public Torquetum API with a directly connected Asterism input and output."""

import argparse
import base64
from contextlib import closing
from getpass import getpass
from http.client import HTTPConnection, HTTPException
import json
import re
import sys
from time import monotonic, sleep
from urllib.parse import quote
from uuid import uuid4

API_PREFIX = "/v0.0"
REQUEST_ID = "X-Tor-Request-Id"


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def invitation():
    code = getpass("Torquetum pairing code: ") if sys.stdin.isatty() else sys.stdin.readline()
    match = re.fullmatch(r"TORAPI\.([\w-]+)", code.strip(), flags=re.ASCII)
    require(match, "Invalid pairing code.")
    encoded = match[1]
    ticket = json.loads(base64.urlsafe_b64decode(encoded + "=" * (-len(encoded) % 4)))
    address = re.fullmatch(r"http://127\.0\.0\.1:([1-9][0-9]{0,4})", ticket["endpoint"])
    require(address and int(address[1]) <= 65535, "The invitation must target IPv4 loopback.")
    return int(address[1]), ticket["invite"]


def geometry(x):
    return {
        "schemaVersion": "0.0", "units": "m", "space": "project",
        "branches": [
            {"path": [2, 5], "items": [{"kind": "point", "position": [x, 0, 0]}, None]},
            {"path": [2, 9], "items": []},
        ],
    }


def until_ready(probe, message):
    deadline = monotonic() + 20
    while monotonic() < deadline:
        result = probe()
        if result is not None:
            return result
        sleep(0.2)
    raise RuntimeError(message)


def smoke(input_selector, output_selector):
    """Replace the test Input twice to verify exact readback, replay, and superseded revisions."""
    port, invite = invitation()
    token = None

    def request(method, path, *, body=None, headers=None, expected=200, pending=None):
        fields = dict(headers or {})
        if token:
            fields["Authorization"] = f"Bearer {token}"
        if body is not None:
            fields["Content-Type"] = "application/json"
        client.request(method, API_PREFIX + path, None if body is None else json.dumps(body), fields)
        with client.getresponse() as response:
            limit = 32 * 1024 * 1024
            raw = response.read(limit + 1)
            require(len(raw) <= limit, "Response exceeds the geometry transport limit.")
            data = json.loads(raw) if raw else {}
            if pending and (response.status, data.get("error", {}).get("code")) == pending:
                return None
            code = data.get("error", {}).get("code", "")
            detail = f" {code}" if isinstance(code, str) and re.fullmatch(r"[A-Z][A-Z_0-9]{0,71}", code) else ""
            require(response.status == expected, f"{method} returned HTTP {response.status}{detail}.")
            return data, response.headers

    def ports():
        return request("GET", "/asterism/ports")[0]["items"]

    def select(items, direction, selector):
        matching = [p for p in items if p["direction"] == direction and p["id"] == selector]
        require(len(matching) == 1, f"Check the ID of the enabled Asterism {direction}.")
        return matching[0]

    def read_receipt(path):
        reply, _ = request("GET", path)
        return None if reply["state"] == "pending" else reply

    with closing(HTTPConnection("127.0.0.1", port, timeout=65)) as client:
        grant, _ = request("POST", "/pair", body={
            "invite": invite, "clientName": "Python Asterism example",
        })
        token = grant["accessToken"]
        project, _ = request("GET", "/project")
        require(all(project[key] == grant[key] for key in ("projectId", "instanceId")), "The paired Project changed.")
        require(project["canWrite"], "A read/write pairing for a standalone project is required.")
        items = ports()
        source = select(items, "input", input_selector)
        target = select(items, "output", output_selector)
        input_path = "/asterism/inputs/" + quote(source["id"], safe="")
        output_path = "/asterism/outputs/" + quote(target["id"], safe="")
        etag = source["etag"]
        previous_id = None

        for x in (1, 2):
            request_id = str(uuid4())
            tree = geometry(x)
            headers = {"If-Match": etag, REQUEST_ID: request_id}
            path = f"/requests/{request_id}"
            location = API_PREFIX + path
            _, reply_headers = request("PUT", input_path, body=tree, headers=headers, expected=202)
            require(reply_headers.get("Location") == location, "The write returned an unexpected receipt location.")
            receipt = until_ready(lambda: read_receipt(path), "Input receipt stayed pending.")
            require(receipt["state"] == "committed", "The input write was rejected after admission.")
            result = receipt["result"]
            require(result["httpStatus"] == 200 and result["id"] == source["id"], "Write targeted the wrong input.")
            require(result["etag"] != etag, "A committed input replacement must change its revision.")
            etag = result["etag"]
            require(select(ports(), "input", source["id"])["etag"] == etag, "Discovery returned a stale input revision.")
            if previous_id is not None:
                superseded, _ = request(
                    "GET", output_path, headers={REQUEST_ID: previous_id}, expected=409,
                )
                require(superseded["error"]["code"] == "INPUT_SUPERSEDED", "A newer write did not supersede the old output.")
            # Asterism can keep evaluating the Output after the Input is committed.
            output, reply_headers = until_ready(
                lambda: request(
                    "GET", output_path, headers={REQUEST_ID: request_id}, pending=(409, "OUTPUT_NOT_READY"),
                ),
                "The connected graph output did not finish.",
            )
            require(output == tree, "The completed output differs from the requested input tree.")
            require(reply_headers.get("ETag") == etag, "The completed output did not verify the receipt's Input version.")
            _, reply_headers = request("PUT", input_path, body=tree, headers=headers, expected=202)
            require(reply_headers.get("Location") == location and read_receipt(path) == receipt, "Retry did not reuse the original receipt.")
            stale, _ = request("PUT", input_path, body=tree, headers={**headers, REQUEST_ID: str(uuid4())}, expected=412)
            require(stale["error"]["code"] == "STALE_REVISION", "A stale write was not rejected.")
            previous_id = request_id


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", help="Geometry Input ID")
    parser.add_argument("output", help="Geometry Output ID")
    args = parser.parse_args()
    try:
        smoke(args.input, args.output)
    except RuntimeError as error:
        sys.exit(str(error))
    except (ValueError, TypeError, KeyError, AttributeError, HTTPException, OSError, EOFError):
        sys.exit("Could not read a valid invitation or response from the local Torquetum API.")
    print("Verified two revisions of the same Asterism input, exact output reads and idempotent retries.")


if __name__ == "__main__":
    main()
