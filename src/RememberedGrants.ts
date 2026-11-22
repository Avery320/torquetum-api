export interface TorApiGrantInfo {
  readonly id: string;
  readonly clientName: string;
  readonly scope: 'read' | 'write';
  readonly lastUsedAt: number;
}

function prefix(projectId: string): string {
  return `torquetum.api.grant.${JSON.stringify(projectId)}.`;
}

export function grantStorage(): Storage | undefined {
  try { return globalThis.window?.localStorage; }
  catch { return undefined; }
}

/** Stores authorization metadata under the reconnect secret's hash. */
export class RememberedGrants {
  constructor(private readonly storage: Storage | undefined) {}

  get(projectId: string, hash: string): TorApiGrantInfo | undefined {
    try {
      const raw = this.storage?.getItem(prefix(projectId) + hash);
      if (!raw) return undefined;
      const value = JSON.parse(raw) as Partial<TorApiGrantInfo> | null;
      if (typeof value?.id === 'string' && typeof value.clientName === 'string'
        && (value.scope === 'read' || value.scope === 'write') && typeof value.lastUsedAt === 'number') {
        return { id: value.id, clientName: value.clientName, scope: value.scope, lastUsedAt: value.lastUsedAt };
      }
    } catch { /* Unavailable or invalid stored grant. */ }
    return undefined;
  }

  list(projectId: string): Map<string, TorApiGrantInfo> {
    const grants = new Map<string, TorApiGrantInfo>();
    const base = prefix(projectId);
    for (let index = 0; index < (this.storage?.length ?? 0); index++) {
      const key = this.storage?.key(index);
      if (!key?.startsWith(base)) continue;
      const hash = key.slice(base.length);
      const grant = this.get(projectId, hash);
      if (grant) grants.set(hash, grant);
    }
    return grants;
  }

  set(projectId: string, hash: string, grant: TorApiGrantInfo): boolean {
    if (!this.storage) return false;
    try {
      this.storage.setItem(prefix(projectId) + hash, JSON.stringify(grant));
      return true;
    } catch { return false; }
  }

  remove(projectId: string, hash: string): void {
    this.storage?.removeItem(prefix(projectId) + hash);
  }
}
