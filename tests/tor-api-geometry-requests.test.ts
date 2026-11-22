import { expect, it, onTestFinished, vi } from 'vitest';
import fixture from '../fixtures/connection.json';
import basicCollection from '../fixtures/geometry-collection-basic.json';
import { TorApiBridge, type TorApiRequest } from '../src/TorApiBridge.js';
import { GeometryAccessError, type TorApiGeometryExchange } from '../src/TorApiGeometryRequests.js';
import { ProjectTorApiSession, type ProjectTorApiAccess } from '../src/ProjectTorApiSession.js';
import bridgeFixture from './fixtures/bridge.json';

const bytes = (value: unknown) => new TextEncoder().encode(JSON.stringify(value));
const collection = (x: number) => ({
  ...basicCollection,
  branches: [{ path: [0], items: [{ kind: 'point', position: [x, 0, 0] }] }],
});
const error = (status: number, code: string) => ({ status, body: { error: { code } } });
const geometryLimits = { maxBranches: 4, maxPathDepth: 2, maxItems: 8, maxVertices: 16, maxTriangles: 12 };
const { maxBodyBytes } = bridgeFixture.bootstrap;

async function setup() {
  let etag = '"original"';
  let output: unknown;
  let staged: unknown;
  const exchange: TorApiGeometryExchange<unknown> = {
    list: () => ({ items: [
      { id: 'in', direction: 'input', etag },
      { id: 'out', direction: 'output' },
    ] }),
    checkWrite(id, ifMatch) {
      if (id !== 'in') throw new GeometryAccessError(404, 'PORT_NOT_FOUND', 'Input unavailable');
      if (ifMatch !== etag) throw new GeometryAccessError(412, 'STALE_REVISION', 'Input changed');
    },
    read(id) {
      if (id !== 'out') throw new GeometryAccessError(404, 'PORT_NOT_FOUND', 'Output unavailable');
      if (!output) throw new GeometryAccessError(409, 'OUTPUT_UNAVAILABLE', 'Press Send on the Output.');
      return output;
    },
    async prepare({ collection }) { return collection; },
    commit(intent, prepared) {
      this.checkWrite(intent.portId, intent.ifMatch);
      staged = prepared;
      etag = '"' + crypto.randomUUID() + '"';
      return { id: intent.portId, etag };
    },
  };
  const replace = vi.spyOn(exchange, 'commit');
  const state = {
    projectId: 'source', projectName: 'Source', busy: false, editing: false,
    mode: 'standalone' as ProjectTorApiAccess, exchange: () => exchange,
  };
  let handler!: Parameters<TorApiBridge['register']>[1];
  const session = new ProjectTorApiSession({
    register: async (_id, listener) => {
      handler = listener;
      return { endpoint: bridgeFixture.bootstrap.endpoint, maxBodyBytes, unregister() {} };
    },
  }, () => state, () => {}, () => crypto.randomUUID(), geometryLimits);
  onTestFinished(() => session.dispose());

  async function pair(readOnly = false) {
    const originalMode = state.mode;
    if (readOnly) state.mode = 'shared-read';
    await session.updateAvailability();
    const code = session.settings().inviteCode.split('.')[1]!;
    const { invite } = JSON.parse(atob(code.replaceAll('-', '+').replaceAll('_', '/')));
    const response = await handler({ route: 'pair', body: bytes({
      invite, clientName: 'Independent client',
    }) });
    state.mode = originalMode;
    expect(response.status).toBe(200);
    expect(response.body).toMatchObject({ canWrite: !readOnly, limits: { ...geometryLimits, maxBodyBytes } });
    return (response.body as { accessToken: string }).accessToken;
  }
  const token = await pair();
  const send = (request: TorApiRequest, bearer = token) => handler({ ...request, bearer });
  const get = (request: Omit<TorApiRequest, 'body'>, bearer?: string) => send({ ...request, body: new Uint8Array() }, bearer);
  const read = (bearer?: string) => get({ route: 'asterism-output', portId: 'out' }, bearer);
  const receipt = (requestId: string, bearer?: string) => get({ route: 'receipt', requestId }, bearer);
  const written = async (id: string, status = 200) => vi.waitFor(async () => {
    const response = await receipt(id);
    expect(response).toMatchObject({ status: 200, body: { result: { httpStatus: status } } });
    return response.body as { result: { httpStatus: number; etag?: string; id?: string; error?: { code: string } } };
  });
  async function write(x: number, portId = 'in'): Promise<TorApiRequest & { requestId: string }> {
    const body = bytes(collection(x));
    const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', body));
    return {
      route: 'asterism-input', body, portId, ifMatch: etag, mediaType: 'application/json', requestId: crypto.randomUUID(),
      bodySha256: Array.from(digest, (byte) => byte.toString(16).padStart(2, '0')).join(''),
    };
  }
  function holdDecode() {
    const prepare = exchange.prepare.bind(exchange);
    let resume!: () => void;
    const gate = new Promise<void>((resolve) => { resume = resolve; });
    vi.spyOn(exchange, 'prepare').mockImplementationOnce(async (...args) => {
      await gate;
      return prepare(...args);
    });
    return resume;
  }
  return {
    state, get, pair, send, replace, write, read, receipt, written, holdDecode,
    reconnect: () => { session.invalidate(); return pair(); },
    changeTag: () => { etag = '"new-instance"'; },
    publish: () => { output = staged; },
  };
}

it('admits one write, returns the same receipt on retry and keeps receipts private to the grant', async () => {
  const app = await setup();
  expect(await app.send(await app.write(1, 'out'))).toMatchObject(error(404, 'PORT_NOT_FOUND'));

  const first = await app.write(1);
  const next = await app.write(2);
  const resume = app.holdDecode();
  const accepted = await app.send(first);
  expect(accepted).toEqual({ status: 202, body: { requestId: first.requestId }, retryAfterSeconds: 1 });
  expect(await app.send(first)).toEqual(accepted);
  expect(await app.send({ ...next, requestId: first.requestId })).toMatchObject(error(409, 'REQUEST_ID_CONFLICT'));
  expect(await app.send(next)).toMatchObject(error(429, 'CAPACITY_EXCEEDED'));
  expect(await app.read()).toMatchObject(error(409, 'OUTPUT_UNAVAILABLE'));
  const reader = await app.pair(true);
  expect(await app.send(next, reader)).toMatchObject(error(403, 'READ_ONLY'));
  expect(await app.receipt(first.requestId, reader)).toMatchObject(error(404, 'REQUEST_UNKNOWN'));

  resume();
  const { result } = await app.written(first.requestId);
  expect(result).toMatchObject({ id: 'in', etag: expect.any(String) });
  expect(await app.send(first)).toEqual(accepted);
  expect(app.replace).toHaveBeenCalledOnce();
});

it('rechecks graph input version and authorization after asynchronous decoding', async () => {
  const app = await setup();
  for (const [change, status, code] of [
    [app.changeTag, 412, 'STALE_REVISION'],
    [() => { app.state.mode = 'shared-read'; }, 403, 'READ_ONLY'],
  ] as const) {
    const request = await app.write(1);
    const resume = app.holdDecode();
    expect(await app.send(request)).toMatchObject({ status: 202 });
    change();
    resume();
    expect((await app.written(request.requestId, status)).result).toMatchObject({ error: { code } });
  }
  expect(await app.read()).toMatchObject(error(409, 'OUTPUT_UNAVAILABLE'));
});

it('lets a new pairing write while an invalidated connection is still decoding', async () => {
  const app = await setup();
  const first = await app.write(1);
  const resume = app.holdDecode();
  expect(await app.send(first)).toMatchObject({ status: 202 });
  try {
    const token = await app.reconnect();
    const next = await app.write(1.493);
    expect(await app.send(next, token)).toMatchObject({ status: 202 });
    await vi.waitFor(async () => {
      expect(await app.receipt(next.requestId, token)).toMatchObject({ status: 200, body: { state: 'committed' } });
    });
    expect(await app.read()).toMatchObject(error(401, 'PAIRING_REQUIRED'));
  } finally {
    resume();
  }
  expect(app.replace).toHaveBeenCalledOnce();
});

it('reads the last published collection independently of staged writes and their receipts', async () => {
  const app = await setup();
  const first = await app.write(1);
  await app.send(first);
  await app.written(first.requestId);
  expect(await app.read()).toMatchObject(error(409, 'OUTPUT_UNAVAILABLE'));
  app.publish();
  const reader = await app.pair(true);
  expect(await app.read(reader)).toEqual({ status: 200, body: collection(1) });

  const next = await app.write(1.493);
  await app.send(next);
  await app.written(next.requestId);
  expect(await app.read(reader)).toEqual({ status: 200, body: collection(1) });
  app.publish();
  expect(await app.read(reader)).toEqual({ status: 200, body: collection(1.493) });
});

it('expires a Project grant and renews pairing against the current Project', async () => {
  vi.useFakeTimers({ toFake: ['Date'] });
  try {
    const app = await setup();
    vi.setSystemTime(Date.now() + 8 * 60 * 60_000);
    expect(await app.get({ route: 'project' })).toMatchObject(error(401, 'PAIRING_REQUIRED'));
    const current = await app.reconnect();
    expect(await app.get({ route: 'project' }, current)).toMatchObject({
      status: 200, body: { projectId: 'source', canRead: true, canWrite: true },
    });
  } finally {
    vi.useRealTimers();
  }
});

it('uses one Studio socket and dispatches a V0.0 request only to its owning Project', async () => {
  class TestSocket extends EventTarget {
    readonly OPEN = 1;
    readyState: 1 | 3 = 1;
    binaryType: BinaryType = 'blob';
    send = vi.fn<WebSocket['send']>();

    receive(data: string | ArrayBuffer): void {
      this.dispatchEvent(new MessageEvent('message', { data }));
    }

    close(): void {
      this.readyState = 3;
      this.dispatchEvent(new Event('close'));
    }
  }

  const socket = new TestSocket();
  const open = vi.fn(() => socket);
  const bridge = new TorApiBridge(async () => bridgeFixture.bootstrap, open);
  onTestFinished(() => bridge.dispose());
  const owner = vi.fn(() => ({ status: 200, body: fixture.pair.response.body }));
  const other = vi.fn(() => ({ status: 200, body: {} }));
  const disconnected = vi.fn();
  const first = bridge.register(bridgeFixture.register.instanceId, owner, disconnected);
  const secondId = 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa';
  const second = bridge.register(secondId, other, disconnected);
  await Promise.resolve();
  socket.dispatchEvent(new Event('open'));
  expect(open).toHaveBeenCalledExactlyOnceWith(bridgeFixture.bootstrap.bridgeUrl);
  expect(socket.send).toHaveBeenCalledExactlyOnceWith(JSON.stringify(bridgeFixture.attach));
  socket.receive(JSON.stringify(bridgeFixture.attached));
  socket.receive(JSON.stringify(bridgeFixture.registered));
  socket.receive(JSON.stringify({ type: 'registered', instanceId: secondId }));
  await Promise.all([first, second]);

  const header = bytes(bridgeFixture.request);
  const body = bytes(fixture.pair.request.body);
  const frame = new Uint8Array(4 + header.byteLength + body.byteLength);
  new DataView(frame.buffer).setUint32(0, header.byteLength);
  frame.set(header, 4);
  frame.set(body, 4 + header.byteLength);
  socket.receive(frame.buffer);
  expect(owner).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ route: 'pair', body }));
  expect(other).not.toHaveBeenCalled();
  await vi.waitFor(() => expect(socket.send.mock.lastCall?.[0]).toBeInstanceOf(Uint8Array));
  socket.close();
  expect(disconnected).toHaveBeenCalledTimes(2);
});
