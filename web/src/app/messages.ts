import { ApiError } from './api/http';

const MESSAGES: Record<string, string> = {
  unauthenticated: 'Your session ended. Sign in again.',
  invalid_credentials: 'That email and password do not match an account.',
  email_unverified: 'Confirm your email first: open the link we sent you.',
  unauthorized: 'Your role in this team does not allow that.',
  tenant_required: 'Pick a team first.',
  not_found: 'Not found.',
  origin_rejected: 'The gateway refused this origin. Check its application origin setting.',
  csrf_failed: 'Your session token is stale. Reload the page.',
  rate_limited: 'Too many attempts. Wait a moment and try again.',
  overloaded: 'The gateway is busy. Try again shortly.',
  starting: 'The gateway is starting. Try again shortly.',
  gateway_unavailable: 'The gateway is not reachable.',
  database_not_found: 'No database of that name in this team.',
  table_not_found: 'No table or view of that name.',
  snapshot_not_found: 'That snapshot no longer exists.',
  database_exists: 'A database with that name already exists in this team.',
  database_limit: 'This team already has 50 active databases. Delete one first.',
  database_expired: 'This database expired. Its data is gone; delete it to reuse the name.',
  database_too_large: 'That would take the database past 1 GiB.',
  not_burstable: 'Only burstable databases can be extended.',
  not_persistent: 'Only persistent databases have snapshots.',
  read_only: 'This statement writes. Switch the console to write mode to run it.',
  statement_denied:
    'memoire does not allow this statement (ATTACH, transaction control, file pragmas…).',
  statement_timeout: 'A statement ran past its deadline and was stopped.',
  result_too_large: 'The result is over 10 000 rows or 16 MiB. Add a LIMIT.',
  sqlite_busy: 'The database is busy with another write. Try again.',
  slug_taken: 'That slug is taken. Pick another.',
  slug_reserved: 'That slug is reserved. Pick another.',
  email_taken: 'An account with that email already exists. Sign in instead.',
  invalid_token: 'This link is invalid, already used or expired.',
  registration_closed: 'This gateway does not accept new accounts.',
  account_disabled: 'This account is disabled.',
  permission_escalation: 'A key cannot hold permissions you do not have yourself.',
  organization_limit: 'You reached the number of teams you can create.',
  service_account_limit: 'This team reached its number of machines.',
  api_key_limit: 'This team reached its number of API keys.',
  reauthentication_required: 'This needs a recent sign-in. Sign out, sign in again, then retry.',
  password_too_short: 'Use a password of at least 12 characters.',
  password_too_long: 'That password is too long.',
  password_blocklisted: 'That password is too common. Pick another.',
  password_contextual: 'The password must not contain your email or the app name.',
  unknown_permission: 'The gateway does not know one of these permissions yet.',
};

const FIELDS: Record<string, string> = {
  invalid: 'is not valid',
  required: 'is required',
  out_of_range: 'is out of range',
  too_long: 'is too long',
  too_short: 'is too short',
  weak: 'is too weak',
  password_too_short: 'needs at least 12 characters',
  password_blocklisted: 'is too common',
  password_contextual: 'must not contain your email or the app name',
};

/** One sentence a person can act on, for any failure. */
export function describe(error: unknown): string {
  if (error instanceof ApiError) {
    const { body } = error;
    if (body.code === 'sql_error' && body.sql) {
      return `Statement ${body.sql.statement + 1}: ${body.sql.message} (${body.sql.sqlite_code})`;
    }
    if (body.code === 'validation_failed' && body.details?.length) {
      return body.details
        .map((d) => `${field(d.path)} ${FIELDS[d.code] ?? `is ${d.code.replace(/_/g, ' ')}`}`)
        .join('; ');
    }
    const known = MESSAGES[body.code];
    if (known) return known;
    if (error.status >= 500) return `The gateway failed (${error.status}). Try again.`;
    return `The gateway refused the request (${body.code}).`;
  }
  if (error instanceof TypeError) return 'The gateway is not reachable.';
  return error instanceof Error ? error.message : String(error);
}

function field(pointer: string): string {
  const name = pointer.replace(/^\//, '').replace(/\//g, '.');
  return name === '' ? 'The request' : name.replace(/_/g, ' ');
}
