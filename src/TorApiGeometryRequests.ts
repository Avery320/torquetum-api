import type { TorApiRequest, TorApiResponse } from './TorApiBridge.js';

interface GeometryDiagnostic {
  readonly path?: readonly number[];
  readonly itemIndex?: number;
  readonly field?: string;
  readonly reason?: string;
}

export class GeometryAccessError extends Error {
  constructor(readonly status: number, readonly code: string, message: string, readonly diagnostic?: GeometryDiagnostic) {
    super(message);
  }
}

export interface GeometryWriteIntent {
  readonly portId: string;
  readonly ifMatch: string;
  readonly collection: unknown;
}

interface CommittedInput {
  readonly id: string;
  readonly etag: string;
}

export interface TorApiGeometryExchange<Prepared> {
  list(): unknown;
  read(portId: string, signal: AbortSignal, expected?: CommittedInput): Promise<unknown>;
  checkWrite(portId: string, ifMatch: string): void;
  prepare(intent: GeometryWriteIntent, signal: AbortSignal): Promise<Prepared>;
  commit(intent: GeometryWriteIntent, prepared: Prepared): CommittedInput;
}

type ErrorDetail = GeometryDiagnostic & { code: string; message: string; requestId?: string };
type ReceiptResult = ({ httpStatus: 200 } & CommittedInput)
  | { httpStatus: number; error: ErrorDetail };

interface Receipt {
  readonly fingerprint: string;
  result?: ReceiptResult;
}

export type TorApiReceipts = Map<string, Receipt>;
interface CollectionAccess {
  readonly receipts: TorApiReceipts;
  readonly abort: AbortController;
}

type AccessGeometry<Prepared> = (mode: 'read' | 'write') => TorApiGeometryExchange<Prepared>;

const MAX_REQUESTS_PER_GRANT = 512;
const decoder = new TextDecoder('utf-8', { fatal: true });

function requireReceipt(receipt: Receipt | undefined): Receipt {
  if (!receipt) throw new GeometryAccessError(404, 'REQUEST_UNKNOWN', 'There is no receipt for this request.');
  return receipt;
}

function committedInput(receipt: Receipt | undefined) {
  const { result } = requireReceipt(receipt);
  if (!result) {
    throw new GeometryAccessError(409, 'OUTPUT_NOT_READY', 'Wait for the geometry write to finish.');
  }
  if ('error' in result) {
    throw new GeometryAccessError(409, 'REQUEST_REJECTED', 'This geometry write was rejected.');
  }
  return result;
}

function geometryError(cause: unknown, requestId?: string): TorApiResponse & { body: { error: ErrorDetail } } {
  const known = cause instanceof GeometryAccessError;
  return {
    status: known ? cause.status : 422, body: { error: {
      code: known ? cause.code : 'INVALID_GEOMETRY',
      message: known ? cause.message : 'Asterism could not exchange the geometry collection.',
      ...(requestId ? { requestId } : {}),
      ...(known ? cause.diagnostic : undefined),
    } },
  };
}

/** A Project holds at most one admitted write, with receipts owned by individual grants. */
export class TorApiGeometryRequests<Prepared> {
  private busy = false;

  async handle(
    request: TorApiRequest, access: AccessGeometry<Prepared>,
    { receipts, abort: { signal } }: CollectionAccess,
  ): Promise<TorApiResponse> {
    try {
      if (request.route === 'asterism-ports') return { status: 200, body: access('read').list() };
      if (request.route === 'asterism-output' && request.portId) {
        const timeout = AbortSignal.timeout(59_000);
        const geometry = access('read');
        const expected = request.requestId ? committedInput(receipts.get(request.requestId)) : undefined;
        try {
          const collection = await geometry.read(request.portId, AbortSignal.any([signal, timeout]), expected);
          access('read');
          return { status: 200, body: collection, ...(expected ? { etag: expected.etag } : {}) };
        } catch (error) {
          if (signal.aborted) access('read');
          if (timeout.aborted) throw new GeometryAccessError(503, 'BRIDGE_UNAVAILABLE', 'Geometry export took too long.');
          throw error;
        }
      }
      const id = request.requestId;
      if (!id) throw new GeometryAccessError(400, 'INVALID_REQUEST', 'A request ID is required.');
      if (request.route === 'receipt') {
        const { result } = requireReceipt(receipts.get(id));
        const state = !result ? 'pending' : 'error' in result ? 'rejected' : 'committed';
        return { status: 200, body: { requestId: id, state, result },
          ...(!result ? { retryAfterSeconds: 1 } : {}) };
      }
      const { portId, ifMatch, mediaType, bodySha256 } = request;
      if (request.route !== 'asterism-input' || !portId || !ifMatch || !bodySha256) {
        throw new GeometryAccessError(400, 'INVALID_REQUEST', 'An Asterism input and its current version are required.');
      }
      const fingerprint = JSON.stringify([portId, ifMatch, mediaType, bodySha256]);
      const accepted = { status: 202, body: { requestId: id }, retryAfterSeconds: 1 };
      const previous = receipts.get(id);
      if (previous) {
        if (previous.fingerprint !== fingerprint) {
          throw new GeometryAccessError(409, 'REQUEST_ID_CONFLICT', 'This request ID already describes a different write.');
        }
        return accepted;
      }
      const geometry = access('write');
      if (this.busy) return {
        ...geometryError(new GeometryAccessError(429, 'CAPACITY_EXCEEDED', 'A write is already in progress.'), id),
        retryAfterSeconds: 1,
      };
      if (receipts.size >= MAX_REQUESTS_PER_GRANT) {
        throw new GeometryAccessError(429, 'PAIRING_LIMIT', 'This pairing has reached its receipt limit.');
      }
      geometry.checkWrite(portId, ifMatch);
      const intent = { portId, ifMatch, collection: this.parseCollection(request.body) };
      const receipt: Receipt = { fingerprint };
      receipts.set(id, receipt);
      this.busy = true;
      void (async () => {
        try {
          const prepared = await geometry.prepare(intent, signal);
          signal.throwIfAborted();
          receipt.result = { httpStatus: 200, ...access('write').commit(intent, prepared) };
        } catch (cause) {
          const failure = geometryError(cause, id);
          receipt.result = { httpStatus: failure.status, error: failure.body.error };
        } finally {
          this.busy = false;
        }
      })();
      return accepted;
    } catch (cause) {
      return geometryError(cause, request.requestId);
    }
  }

  private parseCollection(body: Uint8Array): unknown {
    try { return JSON.parse(decoder.decode(body)); }
    catch { throw new GeometryAccessError(400, 'INVALID_REQUEST', 'Send a valid JSON geometry collection.'); }
  }
}
