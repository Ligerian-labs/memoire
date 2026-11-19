import { expiresAt } from './machines';

describe('expiresAt', () => {
  it('is whole seconds, UTC, the given days ahead', () => {
    expect(expiresAt(Date.parse('2026-11-19T10:00:00.789Z'), 90)).toBe('2027-02-17T10:00:00Z');
  });
});
