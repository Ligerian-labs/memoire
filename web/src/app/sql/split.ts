/**
 * Splits a SQL script into single statements for the query API, which takes
 * exactly one statement per entry. It skips quoted text, quoted identifiers
 * and comments, and keeps a CREATE TRIGGER body (BEGIN … END) in one piece.
 * Empty statements are dropped; the text of each statement is kept as typed.
 */
export function splitStatements(script: string): string[] {
  const statements: string[] = [];
  let start = 0;
  let i = 0;
  let words: string[] = [];
  let depth = 0;
  const push = (end: number) => {
    const text = script.slice(start, end).trim();
    if (stripComments(text).replace(/;/g, '').trim() !== '') statements.push(text);
  };
  while (i < script.length) {
    const c = script[i];
    const next = script[i + 1];
    if (c === "'" || c === '"' || c === '`') {
      i = skipQuoted(script, i, c);
      continue;
    }
    if (c === '[') {
      const close = script.indexOf(']', i + 1);
      i = close === -1 ? script.length : close + 1;
      continue;
    }
    if (c === '-' && next === '-') {
      const nl = script.indexOf('\n', i);
      i = nl === -1 ? script.length : nl + 1;
      continue;
    }
    if (c === '/' && next === '*') {
      const close = script.indexOf('*/', i + 2);
      i = close === -1 ? script.length : close + 2;
      continue;
    }
    if (/[A-Za-z_]/.test(c)) {
      let j = i + 1;
      while (j < script.length && /[A-Za-z0-9_$]/.test(script[j])) j++;
      const word = script.slice(i, j).toUpperCase();
      if (words.length < 4) words.push(word);
      if (isTrigger(words)) {
        if (word === 'BEGIN' || word === 'CASE') depth++;
        else if (word === 'END' && depth > 0) depth--;
      }
      i = j;
      continue;
    }
    if (c === ';' && depth === 0) {
      push(i + 1);
      start = i + 1;
      words = [];
    }
    i++;
  }
  push(script.length);
  return statements;
}

function isTrigger(words: string[]): boolean {
  if (words[0] !== 'CREATE') return false;
  return (
    words[1] === 'TRIGGER' || (/^TEMP(ORARY)?$/.test(words[1] ?? '') && words[2] === 'TRIGGER')
  );
}

function skipQuoted(script: string, i: number, quote: string): number {
  let j = i + 1;
  while (j < script.length) {
    if (script[j] === quote) {
      if (script[j + 1] === quote) {
        j += 2;
        continue;
      }
      return j + 1;
    }
    j++;
  }
  return script.length;
}

function stripComments(text: string): string {
  return text.replace(/--[^\n]*/g, '').replace(/\/\*[\s\S]*?\*\//g, '');
}
