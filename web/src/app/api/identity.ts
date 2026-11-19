import { Http } from './http';
import { Page } from './memoire';

export interface User {
  id: string;
  email: string;
  email_verified: boolean;
  display_name: string | null;
}

export interface Session {
  id: string;
  active_organization_id: string | null;
}

export interface SignedIn {
  session: Session;
  user: User;
  csrf_token: string;
}

export interface Organization {
  id: string;
  slug: string;
  name: string;
  created_at: string;
}

export interface Membership {
  organization: Organization;
}

export interface ServiceAccount {
  id: string;
  name: string;
  description: string | null;
  status: string;
  created_at: string;
}

export interface ApiKey {
  id: string;
  name: string;
  user_id: string | null;
  service_account_id: string | null;
  permissions: string[];
  created_at: string;
  expires_at: string;
  last_used_at: string | null;
  revoked_at: string | null;
}

export interface CreateKey {
  name: string;
  permissions: string[];
  expires_at: string;
  service_account_id?: string;
}

/** The Merezha identity routes the debugger uses (docs/api.md, "Identity routes the clients use"). */
export class IdentityApi {
  constructor(private readonly http: Http) {}

  async session(): Promise<SignedIn> {
    const answer = await this.http.request<SignedIn>('GET', '/v1/identity/session');
    this.http.csrf = answer.csrf_token;
    return answer;
  }

  async signIn(email: string, password: string): Promise<SignedIn> {
    const answer = await this.http.request<SignedIn>('POST', '/v1/identity/sessions', {
      email,
      password,
    });
    this.http.csrf = answer.csrf_token;
    return answer;
  }

  async signOut(): Promise<void> {
    try {
      await this.http.request('DELETE', '/v1/identity/session');
    } finally {
      this.http.csrf = null;
    }
  }

  register(email: string, password: string): Promise<{ status: string }> {
    return this.http.request('POST', '/v1/identity/registrations', { email, password });
  }

  async confirmEmail(token: string): Promise<SignedIn> {
    const answer = await this.http.request<SignedIn>(
      'POST',
      '/v1/identity/email-verifications/confirm',
      { token },
    );
    this.http.csrf = answer.csrf_token;
    return answer;
  }

  organizations(): Promise<Page<Membership>> {
    return this.http.request('GET', '/v1/identity/organizations', undefined, { limit: 100 });
  }

  createOrganization(name: string, slug: string): Promise<Membership> {
    return this.http.request('POST', '/v1/identity/organizations', { name, slug });
  }

  selectOrganization(organizationId: string | null): Promise<{ session: Session }> {
    return this.http.request('PUT', '/v1/identity/session/organization', {
      organization_id: organizationId,
    });
  }

  serviceAccounts(org: string): Promise<Page<ServiceAccount>> {
    return this.http.request('GET', `${orgPath(org)}/service-accounts`, undefined, { limit: 100 });
  }

  createServiceAccount(org: string, name: string, description?: string): Promise<ServiceAccount> {
    const body = description ? { name, description } : { name };
    return this.http.request('POST', `${orgPath(org)}/service-accounts`, body);
  }

  /** Every key of the team in one answer: identity does not page this list. */
  apiKeys(org: string): Promise<{ items: ApiKey[] }> {
    return this.http.request('GET', `${orgPath(org)}/api-keys`);
  }

  createApiKey(org: string, input: CreateKey): Promise<{ api_key: ApiKey; key: string }> {
    return this.http.request('POST', `${orgPath(org)}/api-keys`, input);
  }

  revokeApiKey(org: string, keyId: string): Promise<void> {
    return this.http.request('DELETE', `${orgPath(org)}/api-keys/${encodeURIComponent(keyId)}`);
  }
}

function orgPath(org: string): string {
  return `/v1/identity/organizations/${encodeURIComponent(org)}`;
}
