import { base64Size, cell, parseParams, ParamsError, plain } from './values';

describe('cell', () => {
  it('shows NULL, numbers and text', () => {
    expect(cell(null)).toEqual({ kind: 'null', text: 'NULL' });
    expect(cell(42)).toEqual({ kind: 'number', text: '42' });
    expect(cell('hi')).toEqual({ kind: 'text', text: 'hi' });
  });

  it('keeps big integers and infinities exact as their tagged string', () => {
    expect(cell({ type: 'integer', value: '9223372036854775807' })).toEqual({
      kind: 'big',
      text: '9223372036854775807',
    });
    expect(cell({ type: 'real', value: '-Infinity' }).text).toBe('-Infinity');
  });

  it('summarises blobs with their decoded size', () => {
    const shown = cell({ type: 'blob', value: 'AAE=' });
    expect(shown.kind).toBe('blob');
    expect(shown.text).toBe('blob 2 B AAE=');
  });

  it('shortens long text and keeps the whole value as the title', () => {
    const long = 'x'.repeat(300);
    const shown = cell(long);
    expect(shown.text.length).toBe(201);
    expect(shown.title).toBe(long);
  });
});

describe('base64Size', () => {
  it('accounts for padding', () => {
    expect(base64Size('')).toBe(0);
    expect(base64Size('AA==')).toBe(1);
    expect(base64Size('AAE=')).toBe(2);
    expect(base64Size('AAEC')).toBe(3);
  });
});

describe('plain', () => {
  it('copies the raw value', () => {
    expect(plain(null)).toBe('');
    expect(plain(1.5)).toBe('1.5');
    expect(plain({ type: 'blob', value: 'AAE=' })).toBe('AAE=');
  });
});

describe('parseParams', () => {
  it('returns undefined for an empty field', () => {
    expect(parseParams('  ')).toBeUndefined();
  });

  it('accepts arrays and objects', () => {
    expect(parseParams('[1, "a", null]')).toEqual([1, 'a', null]);
    expect(parseParams('{"id": 2}')).toEqual({ id: 2 });
  });

  it('refuses other JSON and invalid JSON', () => {
    expect(() => parseParams('3')).toThrow(ParamsError);
    expect(() => parseParams('null')).toThrow(ParamsError);
    expect(() => parseParams('[1,')).toThrow(ParamsError);
  });
});
