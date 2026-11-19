import { ApiError } from './api/http';
import { describe as explain } from './messages';

describe('describe', () => {
  it('names the failing statement, its SQLite code and the header message', () => {
    const body = {
      code: 'sql_error',
      details: [{ path: '/statements/1/sql', code: 'sqlite_constraint' }],
    };
    expect(explain(new ApiError(422, body, null, 'UNIQUE constraint failed: users.email'))).toBe(
      'Statement 2: UNIQUE constraint failed: users.email (SQLITE_CONSTRAINT)',
    );
    expect(explain(new ApiError(422, body))).toBe('Statement 2: SQLITE_CONSTRAINT');
  });

  it('lists validation fields without echoing input', () => {
    const error = new ApiError(422, {
      code: 'validation_failed',
      details: [
        { path: '/name', code: 'invalid' },
        { path: '/idle_ttl_seconds', code: 'out_of_range' },
      ],
    });
    expect(explain(error)).toBe('name is not valid; idle ttl seconds is out of range');
  });

  it('maps known codes and falls back on the code itself', () => {
    expect(explain(new ApiError(422, { code: 'read_only' }))).toMatch(/write mode/);
    expect(explain(new ApiError(409, { code: 'something_new' }))).toBe(
      'The gateway refused the request (something_new).',
    );
    expect(explain(new ApiError(500, { code: 'internal' }))).toBe(
      'The gateway failed (500). Try again.',
    );
  });

  it('treats a network failure as an unreachable gateway', () => {
    expect(explain(new TypeError('Failed to fetch'))).toBe('The gateway is not reachable.');
  });
});
