export interface TorApiBootstrap {
  readonly endpoint: string;
  readonly bridgeUrl: string;
  readonly ticket: string;
  readonly maxBodyBytes: number;
}

const ROUTES = ['pair', 'project', 'asterism-ports', 'asterism-output', 'asterism-input', 'receipt'] as const;
type TorApiRoute = typeof ROUTES[number];

export interface TorApiRequest {
  readonly route: TorApiRoute;
  readonly bearer?: string | undefined;
  readonly body: Uint8Array;
  readonly portId?: string | undefined;
  readonly requestId?: string | undefined;
  readonly ifMatch?: string | undefined;
  readonly mediaType?: string | undefined;
  readonly bodySha256?: string | undefined;
  readonly credentialHash?: string | undefined;
}

export interface TorApiResponse {
  readonly status: number;
  readonly body: unknown;
  readonly retryAfterSeconds?: number;
}

interface Registration {
  readonly endpoint: string;
  readonly maxBodyBytes: number;
  readonly locator?: string | undefined;
  unregister(): void;
}

type Socket = Pick<WebSocket, 'OPEN' | 'readyState' | 'binaryType' | 'addEventListener' | 'close' | 'send'>;

interface ProjectRoute {
  readonly projectId: string;
  readonly handle: (request: TorApiRequest) => TorApiResponse | Promise<TorApiResponse>;
  readonly onDisconnect: () => void;
  pending: {
    readonly resolve: (registration: Registration) => void;
    readonly reject: (error: Error) => void;
    readonly timer: ReturnType<typeof setTimeout>;
  } | null;
}

interface Connection {
  readonly events: AbortController;
  readonly projects: Map<string, ProjectRoute>;
  socket: Socket | null;
  attached: boolean;
}

const CONTROL_TIMEOUT_MS = 3000;
const MAX_HEADER_BYTES = 16 * 1024;
const MAX_PAIR_BYTES = 4 * 1024;
const UUID = /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/u;
const encoder = new TextEncoder();
const decoder = new TextDecoder('utf-8', { fatal: true });
const RESPONSE_TOO_LARGE = encoder.encode(JSON.stringify({
  error: { code: 'CONTENT_TOO_LARGE', message: 'The response exceeds the Tor API transfer limit.' },
}));

function bootstrapIsLocal({ endpoint, bridgeUrl, ticket, maxBodyBytes }: TorApiBootstrap): boolean {
  const port = /^http:\/\/127\.0\.0\.1:([1-9][0-9]{0,4})$/u.exec(endpoint)?.[1];
  return port !== undefined && Number(port) <= 65535
    && bridgeUrl === `ws${endpoint.slice(4)}/internal/tor-api/bridge`
    && /^[a-zA-Z0-9_-]{43}$/u.test(ticket)
    && Number.isSafeInteger(maxBodyBytes) && maxBodyBytes >= RESPONSE_TOO_LARGE.byteLength;
}

function jsonObject(input: string | Uint8Array): Record<string, unknown> | null {
  try {
    const value: unknown = JSON.parse(typeof input === 'string' ? input : decoder.decode(input));
    return value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : null;
  } catch {
    return null;
  }
}

// Decode the frame from Rust's request_frame. The relay ID identifies this HTTP exchange.
function readRequest(frame: ArrayBuffer, maxBodyBytes: number) {
  if (frame.byteLength < 4) return null;
  const headerLength = new DataView(frame).getUint32(0);
  const bodyStart = 4 + headerLength;
  if (headerLength > MAX_HEADER_BYTES || bodyStart > frame.byteLength
    || frame.byteLength - bodyStart > maxBodyBytes) return null;
  const header = jsonObject(new Uint8Array(frame, 4, headerLength));
  if (!header) return null;
  const { type, route, relayId, instanceId, bearer, portId, requestId, ifMatch, mediaType, bodySha256, credentialHash } = header;
  if (type !== 'request'
    || typeof relayId !== 'string' || !UUID.test(relayId)
    || typeof instanceId !== 'string' || !UUID.test(instanceId)
    || !ROUTES.includes(route as TorApiRoute)
    || (route !== 'pair' && route !== 'asterism-input' && frame.byteLength !== bodyStart)
    || (route === 'pair' && frame.byteLength - bodyStart > MAX_PAIR_BYTES)
    || (bearer !== undefined && typeof bearer !== 'string')
    || (portId !== undefined && typeof portId !== 'string')
    || (requestId !== undefined && (typeof requestId !== 'string' || !UUID.test(requestId)))
    || (ifMatch !== undefined && typeof ifMatch !== 'string')
    || (mediaType !== undefined && typeof mediaType !== 'string')
    || (bodySha256 !== undefined && (typeof bodySha256 !== 'string' || !/^[a-f0-9]{64}$/u.test(bodySha256)))
    || (credentialHash !== undefined && (route !== 'pair' || typeof credentialHash !== 'string' || !/^[a-f0-9]{64}$/u.test(credentialHash)))) return null;
  const request: TorApiRequest = {
    route: route as TorApiRoute,
    body: new Uint8Array(frame, bodyStart),
    bearer, portId, requestId, ifMatch, mediaType, bodySha256, credentialHash,
  };
  return { instanceId, relayId, request };
}

function encodeResponse(relayId: string, response: TorApiResponse, maxBodyBytes: number): Uint8Array<ArrayBuffer> {
  const { body, status, retryAfterSeconds } = response;
  const text = JSON.stringify(body);
  if (text === undefined || !Number.isInteger(status) || status < 200 || status > 599) {
    throw new Error('Invalid Tor API response.');
  }
  const payload = text.length <= maxBodyBytes ? encoder.encode(text) : undefined;
  const tooLarge = !payload || payload.byteLength > maxBodyBytes;
  const bodyBytes = tooLarge ? RESPONSE_TOO_LARGE : payload;
  const header = encoder.encode(JSON.stringify({
    type: 'response', relayId, status: tooLarge ? 413 : status, mediaType: 'application/json',
    ...(tooLarge ? {} : { retryAfterSeconds }),
  }));
  const frame = new Uint8Array(4 + header.byteLength + bodyBytes.byteLength);
  new DataView(frame.buffer).setUint32(0, header.byteLength);
  frame.set(header, 4);
  frame.set(bodyBytes, 4 + header.byteLength);
  return frame;
}

/** One socket per Studio shell. Project runtimes each own a registration. */
export class TorApiBridge {
  private connection: Connection | null = null;
  private disposed = false;

  constructor(
    private readonly bootstrap: () => Promise<TorApiBootstrap>,
    private readonly socketFactory: (url: string) => Socket = (url) => new WebSocket(url),
  ) {}

  /** Wait for host acknowledgement before the Project can issue pairing codes for this route. */
  async register(instanceId: string, projectId: string, handle: ProjectRoute['handle'], onDisconnect: () => void): Promise<Registration> {
    if (this.disposed || !UUID.test(instanceId)) throw new Error('Invalid Tor API registration.');
    const open = !this.connection;
    const connection = this.connection ??= {
      events: new AbortController(),
      projects: new Map(),
      socket: null,
      attached: false,
    };
    if (connection.projects.has(instanceId)) throw new Error('Project is already registered with Tor API.');
    const registered = new Promise<Registration>((resolve, reject) => {
      const timer = setTimeout(() => this.disconnect(connection), CONTROL_TIMEOUT_MS);
      connection.projects.set(instanceId, { projectId, handle, onDisconnect, pending: { resolve, reject, timer } });
    });
    if (open) void this.connect(connection);
    else if (connection.attached) this.send(connection, { type: 'register', instanceId, projectId });
    return registered;
  }

  dispose(): void {
    this.disposed = true;
    if (this.connection) this.disconnect(this.connection);
  }

  private async connect(connection: Connection): Promise<void> {
    try {
      const bootstrap = await this.bootstrap();
      if (this.connection !== connection) return;
      if (!bootstrapIsLocal(bootstrap)) throw new Error('Invalid Tor API bootstrap.');
      const socket = connection.socket = this.socketFactory(bootstrap.bridgeUrl);
      socket.binaryType = 'arraybuffer';
      const eventOptions = { signal: connection.events.signal };
      socket.addEventListener('open', () => {
        this.send(connection, { type: 'attach', ticket: bootstrap.ticket });
      }, { ...eventOptions, once: true });
      socket.addEventListener('message', ({ data }) => this.receive(connection, data, bootstrap), eventOptions);
      socket.addEventListener('error', () => this.disconnect(connection), eventOptions);
      socket.addEventListener('close', () => this.disconnect(connection), eventOptions);
    } catch {
      this.disconnect(connection);
    }
  }

  private send(connection: Connection, data: object | Uint8Array<ArrayBuffer>): void {
    if (this.connection !== connection) return;
    try {
      const socket = connection.socket;
      if (!socket || socket.readyState !== socket.OPEN) throw new Error('Tor API socket is not open.');
      socket.send(data instanceof Uint8Array ? data : JSON.stringify(data));
    } catch {
      this.disconnect(connection);
    }
  }

  private receive(connection: Connection, data: unknown, bootstrap: TorApiBootstrap): void {
    if (this.connection !== connection) return;
    if (typeof data === 'string') {
      if (data.length > MAX_HEADER_BYTES || encoder.encode(data).byteLength > MAX_HEADER_BYTES) return;
      const control = jsonObject(data);
      if (control?.type === 'attached' && !connection.attached) {
        connection.attached = true;
        for (const [instanceId, { projectId }] of connection.projects) this.send(connection, { type: 'register', instanceId, projectId });
      } else if (control?.type === 'registered' && connection.attached && typeof control.instanceId === 'string') {
        const locator = typeof control.locator === 'string' && /^[a-f0-9]{64}$/u.test(control.locator) ? control.locator : undefined;
        this.acknowledge(connection, control.instanceId, bootstrap, locator);
      }
    } else if (connection.attached && data instanceof ArrayBuffer) {
      const relay = readRequest(data, bootstrap.maxBodyBytes);
      if (relay) void this.reply(connection, relay, bootstrap.maxBodyBytes);
    }
  }

  private acknowledge(connection: Connection, instanceId: string, { endpoint, maxBodyBytes }: TorApiBootstrap, locator?: string): void {
    const project = connection.projects.get(instanceId);
    if (!project?.pending) return;
    const { resolve, timer } = project.pending;
    project.pending = null;
    clearTimeout(timer);
    resolve({
      endpoint, maxBodyBytes, locator,
      unregister: () => {
        if (connection.projects.get(instanceId) !== project) return;
        connection.projects.delete(instanceId);
        this.send(connection, { type: 'unregister', instanceId });
      },
    });
  }

  private async reply(
    connection: Connection, { instanceId, relayId, request }: NonNullable<ReturnType<typeof readRequest>>, maxBodyBytes: number,
  ) {
    const project = connection.projects.get(instanceId);
    if (!project || project.pending) return;
    let response: Uint8Array<ArrayBuffer>;
    try {
      response = encodeResponse(relayId, await project.handle(request), maxBodyBytes);
    } catch {
      response = encodeResponse(relayId, {
        status: 503,
        body: { error: { code: 'BRIDGE_UNAVAILABLE', message: 'The Studio bridge could not complete the request.' } },
      }, maxBodyBytes);
    }
    // The Project may have unregistered while its handler was running.
    if (this.connection === connection && connection.projects.get(instanceId) === project) this.send(connection, response);
  }

  private disconnect(connection: Connection): void {
    if (this.connection !== connection) return;
    this.connection = null;
    connection.events.abort();
    connection.socket?.close();
    const projects = [...connection.projects.values()];
    connection.projects.clear();
    for (const { pending, onDisconnect } of projects) {
      if (pending) {
        clearTimeout(pending.timer);
        pending.reject(new Error('Tor API bridge is unavailable.'));
        continue;
      }
      try {
        onDisconnect();
      } catch {
        // One Project cannot interrupt cleanup of the other registered routes.
      }
    }
  }
}
