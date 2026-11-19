import { bytes, DATABASE_NAME, duration, relative, slugify, timestamp } from './format';

describe('format', () => {
  it('bytes', () => {
    expect(bytes(null)).toBe('—');
    expect(bytes(512)).toBe('512 B');
    expect(bytes(8192)).toBe('8.0 KiB');
    expect(bytes(1024 ** 3)).toBe('1.0 GiB');
    expect(bytes(300 * 1024 * 1024)).toBe('300 MiB');
  });

  it('duration keeps the two largest units', () => {
    expect(duration(0)).toBe('0s');
    expect(duration(45)).toBe('45s');
    expect(duration(3600)).toBe('1h');
    expect(duration(93784)).toBe('1d 2h');
    expect(duration(86400 + 30)).toBe('1d');
  });

  it('relative', () => {
    const now = Date.parse('2026-11-19T10:00:00Z');
    expect(relative(null, now)).toBe('—');
    expect(relative('2026-11-19T10:00:02Z', now)).toBe('now');
    expect(relative('2026-11-19T13:05:00Z', now)).toBe('in 3h 5m');
    expect(relative('2026-11-19T09:48:00Z', now)).toBe('12m ago');
  });

  it('timestamp prints UTC', () => {
    expect(timestamp('2026-11-19T10:05:12.345Z')).toBe('2026-11-19 10:05:12Z');
    expect(timestamp(null)).toBe('—');
  });

  it('slugify', () => {
    expect(slugify('  Équipe Data & ML ')).toBe('equipe-data-ml');
  });

  it('database names follow the contract', () => {
    expect(DATABASE_NAME.test('scratch')).toBe(true);
    expect(DATABASE_NAME.test('a')).toBe(false);
    expect(DATABASE_NAME.test('1db')).toBe(false);
    expect(DATABASE_NAME.test('Upper')).toBe(false);
    expect(DATABASE_NAME.test('a'.repeat(64))).toBe(false);
  });
});
