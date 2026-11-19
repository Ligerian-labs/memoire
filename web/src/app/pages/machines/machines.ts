import { Component, computed, effect, inject, signal, untracked } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Api } from '../../api/api';
import { ApiKey, ServiceAccount } from '../../api/identity';
import { relative, timestamp } from '../../format';
import { describe } from '../../messages';
import { SessionStore } from '../../session';
import { Modal } from '../../ui/modal';

export const PERMISSIONS = [
  { id: 'memoire.databases.read', label: 'Read', hint: 'list, read queries, schema, rows, export' },
  { id: 'memoire.databases.write', label: 'Write', hint: 'write queries, extend, import' },
  { id: 'memoire.databases.create', label: 'Create', hint: 'create databases' },
  { id: 'memoire.databases.delete', label: 'Delete', hint: 'delete databases' },
  { id: 'memoire.databases.snapshots', label: 'Snapshots', hint: 'restore snapshots' },
];
export const EXPIRY_DAYS = [30, 90, 365];

/** `expires_at` for a key valid `days` from `now`, at whole seconds. */
export function expiresAt(now: number, days: number): string {
  return new Date(Math.floor(now / 1000) * 1000 + days * 86_400_000)
    .toISOString()
    .replace('.000Z', 'Z');
}

@Component({
  selector: 'app-machines',
  imports: [FormsModule, Modal],
  templateUrl: './machines.html',
  styleUrl: './machines.css',
})
export class Machines {
  private readonly api = inject(Api);
  readonly session = inject(SessionStore);
  readonly machines = signal<ServiceAccount[]>([]);
  readonly keys = signal<ApiKey[]>([]);
  readonly loading = signal(true);
  readonly error = signal('');
  readonly showRevoked = signal(false);
  readonly visibleKeys = computed(() =>
    this.showRevoked() ? this.keys() : this.keys().filter((k) => !k.revoked_at),
  );
  readonly machineNames = computed(
    () => new Map(this.machines().map((m) => [m.id, m.name] as const)),
  );
  readonly dialog = signal<'machine' | 'key' | null>(null);
  readonly busy = signal(false);
  readonly formError = signal('');
  readonly minted = signal<{ name: string; key: string } | null>(null);
  readonly copied = signal(false);
  readonly revoking = signal<ApiKey | null>(null);
  readonly permissions = PERMISSIONS;
  readonly expiryDays = EXPIRY_DAYS;
  readonly timestamp = timestamp;
  readonly relative = relative;
  readonly now = signal(Date.now());
  readonly origin = location.origin;

  machineName = '';
  machineDescription = '';
  keyName = '';
  keyOwner = '';
  keyAccess: 'full' | 'read' = 'full';
  keyDays = 90;

  constructor() {
    effect(() => {
      this.session.teamId();
      untracked(() => void this.load());
    });
  }

  private get org(): string {
    return this.session.teamId() ?? '';
  }

  async load(): Promise<void> {
    if (!this.org) return;
    this.loading.set(true);
    this.error.set('');
    try {
      const [machines, keys] = await Promise.all([
        this.api.identity.serviceAccounts(this.org),
        this.api.identity.apiKeys(this.org),
      ]);
      this.machines.set(machines.items);
      this.keys.set(keys.items);
      this.now.set(Date.now());
    } catch (error) {
      this.error.set(describe(error));
    } finally {
      this.loading.set(false);
    }
  }

  owner(key: ApiKey): string {
    if (key.service_account_id) return this.machineNames().get(key.service_account_id) ?? 'machine';
    return key.user_id === this.session.user()?.id ? 'you' : 'member';
  }

  access(key: ApiKey): string {
    const memoire = key.permissions.filter((p) => p.startsWith('memoire.'));
    if (memoire.length === PERMISSIONS.length) return 'full';
    if (memoire.length === 1 && memoire[0] === 'memoire.databases.read') return 'read-only';
    return memoire.map((p) => p.replace('memoire.databases.', '')).join(', ') || 'none';
  }

  openMachine(): void {
    this.machineName = '';
    this.machineDescription = '';
    this.formError.set('');
    this.dialog.set('machine');
  }

  openKey(owner = ''): void {
    this.keyName = '';
    this.keyOwner = owner;
    this.keyAccess = 'full';
    this.keyDays = 90;
    this.formError.set('');
    this.minted.set(null);
    this.copied.set(false);
    this.dialog.set('key');
  }

  closeDialog(): void {
    this.dialog.set(null);
    this.minted.set(null);
  }

  async createMachine(): Promise<void> {
    await this.submit(async () => {
      const org = this.org;
      // A machine acts only through a team role; its keys carry a subset of the role's permissions.
      const roles = await this.api.identity.roles(org);
      const member = roles.items.find((r) => r.key === 'member');
      if (!member) throw new Error('This team has no member role to give the machine.');
      const machine = await this.api.identity.createServiceAccount(
        org,
        this.machineName.trim(),
        this.machineDescription.trim() || undefined,
      );
      this.machines.update((m) => [...m, machine]);
      await this.api.identity.assignRole(org, machine.id, member.id);
      this.openKey(machine.id);
      this.keyName = machine.name;
    }, false);
  }

  async createKey(): Promise<void> {
    await this.submit(async () => {
      const permissions =
        this.keyAccess === 'read' ? ['memoire.databases.read'] : PERMISSIONS.map((p) => p.id);
      const answer = await this.api.identity.createApiKey(this.org, {
        name: this.keyName.trim(),
        permissions,
        expires_at: expiresAt(Date.now(), this.keyDays),
        ...(this.keyOwner ? { service_account_id: this.keyOwner } : {}),
      });
      this.keys.update((k) => [answer.api_key, ...k]);
      this.minted.set({ name: answer.api_key.name, key: answer.key });
    }, false);
  }

  async revoke(key: ApiKey): Promise<void> {
    await this.submit(async () => {
      await this.api.identity.revokeApiKey(this.org, key.id);
      this.revoking.set(null);
      await this.load();
    }, false);
  }

  async copy(text: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(text);
      this.copied.set(true);
    } catch {
      this.copied.set(false);
    }
  }

  private async submit(work: () => Promise<void>, close: boolean): Promise<void> {
    this.busy.set(true);
    this.formError.set('');
    try {
      await work();
      if (close) this.dialog.set(null);
    } catch (error) {
      this.formError.set(describe(error));
    } finally {
      this.busy.set(false);
    }
  }
}
