import schema from '../contracts/geometry-collection.schema.json';
import {
  GeometryAccessError, TorApiGeometryRequests, type TorApiGeometryExchange, type TorApiReceipts,
} from './TorApiGeometryRequests.js';
import type { TorApiBridge, TorApiRequest, TorApiResponse } from './TorApiBridge.js';
import { RememberedGrants, grantStorage, type TorApiGrantInfo } from './RememberedGrants.js';

const INVITE_TTL_MS = 5 * 60_000;
const GRANT_TTL_MS = 8 * 60 * 60_000;
const MAX_GRANTS = 32;
const GEOMETRY_KINDS = Object.freeze(schema.$defs.geometry.oneOf.map(({ properties }) => properties.kind.const));

type TorApiScope = TorApiGrantInfo['scope'];
export type ProjectTorApiAccess = 'standalone' | 'pending' | 'shared-read';

interface ProjectAccess<Prepared> {
  readonly projectId: string;
  readonly projectName: string;
  readonly mode: ProjectTorApiAccess;
  readonly busy: boolean;
  readonly editing: boolean;
  readonly exchange?: () => TorApiGeometryExchange<Prepared>;
}

type GeometryLimits = Readonly<Record<'maxBranches' | 'maxPathDepth' | 'maxItems' | 'maxVertices' | 'maxTriangles', number>>;

interface Invite {
  readonly secret: string;
  readonly code: string;
  readonly scope: TorApiScope;
  readonly expiresAt: number;
  readonly timer: ReturnType<typeof setTimeout>;
}

interface Grant extends TorApiGrantInfo {
  readonly expiresAt: number;
  lastUsedAt: number;
  readonly rememberedHash?: string | undefined;
  readonly receipts: TorApiReceipts;
  readonly abort: AbortController;
}

interface Connection<Prepared> {
  readonly instanceId: string;
  readonly projectId: string;
  readonly geometryRequests: TorApiGeometryRequests<Prepared>;
  pending?: Promise<void> | undefined;
  registration?: Awaited<ReturnType<TorApiBridge['register']>>;
}

export interface TorApiSettings {
  readonly inviteCode: string;
  readonly grants: readonly TorApiGrantInfo[];
}

const encodeBase64Url = (bytes: Uint8Array): string => (
  btoa(String.fromCharCode(...bytes)).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/u, '')
);
function newSecret(instanceId: string): string {
  const bytes = new Uint8Array(48);
  const uuid = instanceId.replaceAll('-', '');
  for (let index = 0; index < 16; index++) bytes[index] = Number.parseInt(uuid.slice(index * 2, index * 2 + 2), 16);
  crypto.getRandomValues(bytes.subarray(16));
  return encodeBase64Url(bytes);
}

const error = (status: number, code: string, message: string): TorApiResponse => ({
  status, body: { error: { code, message } },
});
const invalidInvite = () => error(401, 'INVITE_INVALID', 'Get a new Tor API invitation from the intended project.');
const invalidGrant = () => error(401, 'PAIRING_REQUIRED', 'Pair again with the intended project.');
const readyForPairing = ({ mode, busy, editing }: ProjectAccess<unknown>): boolean => (
  mode !== 'pending' && !busy && !editing
);
const allowsWrite = ({ mode }: ProjectAccess<unknown>, scope: TorApiScope): boolean => mode === 'standalone' && scope === 'write';

function parsePair(body: Uint8Array): { credential: string; clientName: string; remember?: string } | undefined {
  try {
    const value = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(body));
    if (typeof value?.credential !== 'string' || typeof value.clientName !== 'string'
      || value.clientName.length > 80 || !value.clientName.trim()
      || Object.keys(value).some((key) => key !== 'credential' && key !== 'clientName' && key !== 'remember')
      || (value.remember !== undefined && (typeof value.remember !== 'string' || !/^[a-f0-9]{64}$/u.test(value.remember)))) return undefined;
    return value;
  } catch { return undefined; }
}

/**
 * Keeps one Project's invites and grants when Studio opens its draft.
 * Replacing the connection revokes that instance's grants and cancels their work.
 */
export class ProjectTorApiSession<Prepared> {
  private connection?: Connection<Prepared> | undefined;
  private invite?: Invite | undefined;
  private readonly grants = new Map<string, Grant>();
  private readonly remembered: RememberedGrants;
  private disposed = false;

  constructor(
    private readonly bridge: Pick<TorApiBridge, 'register'>,
    private readonly readProject: () => ProjectAccess<Prepared> | undefined,
    private readonly changed: () => void,
    private readonly createUuid: () => string,
    private readonly geometryLimits: GeometryLimits,
    storage = grantStorage(),
  ) { this.remembered = new RememberedGrants(storage); }

  updateAvailability(): Promise<void> {
    const connection = this.connect();
    this.changed();
    return connection;
  }

  invalidate(): void {
    const old = this.connection;
    this.connection = undefined;
    this.clearInvite();
    for (const grant of this.grants.values()) grant.abort.abort();
    this.grants.clear();
    old?.registration?.unregister();
  }

  dispose(): void {
    this.disposed = true;
    this.invalidate();
  }

  settings(): TorApiSettings {
    this.expire();
    const project = this.readProject();
    const connection = this.connection;
    const inviteCode = project && connection?.registration && project.projectId === connection.projectId
      && readyForPairing(project)
      ? this.invitationCode(connection.instanceId, connection.registration.endpoint,
        project.mode === 'standalone' ? 'write' : 'read')
      : '';
    return {
      inviteCode,
      grants: project ? this.grantList(project.projectId) : [],
    };
  }

  private grantList(projectId: string): TorApiGrantInfo[] {
    const grants = new Map([...this.remembered.list(projectId).values()].map((grant) => [grant.id, grant]));
    for (const { id, clientName, scope, lastUsedAt } of this.grants.values()) {
      grants.set(id, { id, clientName, scope, lastUsedAt });
    }
    return [...grants.values()];
  }

  private invitationCode(instanceId: string, endpoint: string, scope: TorApiScope): string {
    if (this.invite?.scope === scope) return this.invite.code;
    this.clearInvite();
    const secret = newSecret(instanceId);
    const bytes = new TextEncoder().encode(JSON.stringify({ endpoint, invite: secret }));
    const code = 'TORAPI.' + encodeBase64Url(bytes);
    const timer = setTimeout(() => {
      this.invite = undefined;
      this.changed();
    }, INVITE_TTL_MS);
    this.invite = { secret, code, scope, expiresAt: Date.now() + INVITE_TTL_MS, timer };
    return code;
  }

  private clearInvite(): void {
    clearTimeout(this.invite?.timer);
    this.invite = undefined;
  }

  revoke(id: string): void {
    const project = this.readProject();
    if (project) {
      for (const [hash, grant] of this.remembered.list(project.projectId)) {
        if (grant.id === id) this.remembered.remove(project.projectId, hash);
      }
    }
    for (const [token, grant] of this.grants) {
      if (grant.id !== id) continue;
      this.grants.delete(token);
      grant.abort.abort();
    }
    this.changed();
  }

  private connect(): Promise<void> {
    const project = this.readProject();
    if (this.connection && project?.projectId !== this.connection.projectId) this.invalidate();
    if (this.disposed || !project || project.mode === 'pending') return Promise.resolve();
    if (this.connection) return this.connection.pending ?? Promise.resolve();
    const connection: Connection<Prepared> = {
      projectId: project.projectId, instanceId: this.createUuid(), geometryRequests: new TorApiGeometryRequests(),
    };
    this.connection = connection;
    connection.pending = this.bridge.register(connection.instanceId, connection.projectId, (request) => this.handle(connection, request), () => {
      if (this.connection !== connection) return;
      this.invalidate();
      void this.updateAvailability();
    }).then((registration) => {
      // Acknowledgement can arrive after the Project has closed or changed.
      if (this.connection !== connection || this.readProject()?.projectId !== connection.projectId) {
        registration.unregister();
        return;
      }
      connection.registration = registration;
    }, () => {
      if (this.connection === connection) this.invalidate();
    }).finally(() => {
      connection.pending = undefined;
      if (!this.disposed) this.changed();
    });
    return connection.pending;
  }

  private expire(): void {
    const now = Date.now();
    const projectId = this.connection?.projectId;
    if (this.invite && this.invite.expiresAt <= now) this.clearInvite();
    for (const [token, grant] of this.grants) {
      if (grant.expiresAt > now && (!projectId || this.isRememberedGrantCurrent(projectId, grant))) continue;
      this.grants.delete(token);
      grant.abort.abort();
    }
  }

  private isRememberedGrantCurrent(projectId: string, grant: Grant): boolean {
    return !grant.rememberedHash || this.remembered.get(projectId, grant.rememberedHash)?.id === grant.id;
  }

  private handle(connection: Connection<Prepared>, request: TorApiRequest): TorApiResponse | Promise<TorApiResponse> {
    const { instanceId, registration } = connection;
    const project = this.projectFor(connection);
    const pairing = request.route === 'pair';
    if (!project || !registration) return pairing ? invalidInvite() : invalidGrant();
    const { maxBodyBytes } = registration;
    this.expire();
    const now = Date.now();
    if (pairing) {
      const input = parsePair(request.body);
      if (!input) return error(400, 'INVALID_REQUEST', 'Invalid Tor API pairing request.');
      const saved = request.credentialHash && this.remembered.get(project.projectId, request.credentialHash);
      const invite = this.invite?.secret === input.credential ? this.invite : undefined;
      const scope = saved ? saved.scope : invite?.scope;
      if (!scope) return invalidInvite();
      if (project.mode === 'pending') return error(503, 'PROJECT_SYNC_PENDING', 'Wait for the project to finish synchronizing.');
      if (!readyForPairing(project)) return error(409, 'PROJECT_BUSY', 'Wait for the current project operation.');
      if (!saved && scope === 'write' && !allowsWrite(project, scope)) {
        return error(403, 'READ_ONLY', 'Create a read-only invitation for this shared project.');
      }
      if (!saved && this.grantList(project.projectId).length >= MAX_GRANTS) {
        return error(429, 'PAIRING_LIMIT', 'Revoke an old Tor API pairing first.');
      }
      const info = saved || {
        id: this.createUuid(), clientName: input.clientName.trim(), scope, lastUsedAt: now,
      };
      const rememberedHash = saved ? request.credentialHash : input.remember;
      if (rememberedHash) {
        if (!registration.locator || (!saved && (this.remembered.get(project.projectId, rememberedHash)
          || !this.remembered.set(project.projectId, rememberedHash, info)))) {
          return error(503, 'PAIRING_STORAGE_UNAVAILABLE', 'Torquetum could not remember this connection.');
        }
        for (const [token, previous] of this.grants) {
          if (previous.id !== info.id) continue;
          this.grants.delete(token);
          previous.abort.abort();
        }
      }
      const accessToken = newSecret(instanceId);
      const grant: Grant = {
        ...info, lastUsedAt: now, expiresAt: now + GRANT_TTL_MS, rememberedHash,
        receipts: new Map(), abort: new AbortController(),
      };
      this.grants.set(accessToken, grant);
      if (invite) this.clearInvite();
      this.changed();
      return { status: 200, body: {
        ...this.info(project, grant.scope, instanceId, maxBodyBytes),
        grantId: grant.id, accessToken, expiresAt: new Date(grant.expiresAt).toISOString(),
        ...(rememberedHash ? { locator: registration.locator } : {}),
      } };
    }
    const token = request.bearer ?? '';
    const grant = this.grants.get(token);
    if (!grant) return invalidGrant();
    grant.lastUsedAt = now;
    this.changed();
    if (request.route === 'project') return { status: 200, body: this.info(project, grant.scope, instanceId, maxBodyBytes) };
    return connection.geometryRequests.handle(request, (mode) => this.accessExchange(connection, token, grant, mode), grant);
  }

  private projectFor(connection: Connection<Prepared>): ProjectAccess<Prepared> | undefined {
    if (this.connection !== connection) return undefined;
    const project = this.readProject();
    return project?.projectId === connection.projectId ? project : undefined;
  }

  private accessExchange(connection: Connection<Prepared>, token: string, grant: Grant, access: 'read' | 'write') {
    const project = this.projectFor(connection);
    if (!project || this.grants.get(token) !== grant || grant.expiresAt <= Date.now()
      || !this.isRememberedGrantCurrent(project.projectId, grant)) {
      throw new GeometryAccessError(401, 'PAIRING_REQUIRED', 'Pair again with the intended project.');
    }
    if (project.mode === 'pending') {
      throw new GeometryAccessError(503, 'PROJECT_SYNC_PENDING', 'Wait for the project to finish synchronizing.');
    }
    if (!project.exchange) {
      throw new GeometryAccessError(409, 'PROJECT_NOT_RUNNING', 'Open the paired Project in Studio to exchange geometry.');
    }
    if (project.editing) throw new GeometryAccessError(409, 'PROJECT_BUSY', 'Finish the current geometry edit first.');
    if (access === 'write') {
      if (!allowsWrite(project, grant.scope)) {
        throw new GeometryAccessError(403, 'READ_ONLY', 'This project is available for read-only access.');
      }
      if (project.busy) throw new GeometryAccessError(409, 'PROJECT_BUSY', 'Wait for the current project operation.');
    }
    return project.exchange();
  }

  private info(project: ProjectAccess<Prepared>, scope: TorApiScope, instanceId: string, maxBodyBytes: number) {
    const { projectId, projectName, mode, busy, editing, exchange } = project;
    const canRead = mode !== 'pending' && !editing;
    const canWrite = allowsWrite(project, scope) && readyForPairing(project);
    const availabilityReason = mode === 'pending' ? 'PROJECT_SYNC_PENDING'
      : busy || editing ? 'PROJECT_BUSY'
      : !exchange ? 'PROJECT_NOT_RUNNING'
      : canWrite ? undefined : 'READ_ONLY';
    return {
      projectId, projectName, instanceId,
      canRead, canWrite, availabilityReason,
      kinds: GEOMETRY_KINDS,
      mediaTypes: ['application/json'],
      limits: { ...this.geometryLimits, maxBodyBytes },
    };
  }
}
