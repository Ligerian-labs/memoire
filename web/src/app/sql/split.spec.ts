import { splitStatements } from './split';

describe('splitStatements', () => {
  it('splits on semicolons and drops empty statements', () => {
    expect(splitStatements('SELECT 1; ;\n SELECT 2;')).toEqual(['SELECT 1;', 'SELECT 2;']);
  });

  it('keeps a last statement without a semicolon', () => {
    expect(splitStatements('CREATE TABLE t (a);\nINSERT INTO t VALUES (1)')).toEqual([
      'CREATE TABLE t (a);',
      'INSERT INTO t VALUES (1)',
    ]);
  });

  it('ignores semicolons in strings, identifiers and comments', () => {
    const script = `INSERT INTO "a;b" VALUES ('x;''y'); -- c;d\nSELECT [e;f], \`g;h\` /* i;j */ FROM t`;
    expect(splitStatements(script)).toEqual([
      `INSERT INTO "a;b" VALUES ('x;''y');`,
      '-- c;d\nSELECT [e;f], `g;h` /* i;j */ FROM t',
    ]);
  });

  it('drops a trailing comment-only fragment', () => {
    expect(splitStatements('SELECT 1; -- done')).toEqual(['SELECT 1;']);
  });

  it('keeps a trigger body in one statement, CASE … END included', () => {
    const trigger = `CREATE TEMP TRIGGER log AFTER INSERT ON t BEGIN
  INSERT INTO l VALUES (CASE WHEN new.a > 0 THEN 'p' ELSE 'n' END);
  UPDATE c SET n = n + 1;
END;`;
    expect(splitStatements(`${trigger}\nSELECT 1`)).toEqual([trigger, 'SELECT 1']);
  });

  it('does not treat a lone BEGIN as a block', () => {
    expect(splitStatements('BEGIN; SELECT 1;')).toEqual(['BEGIN;', 'SELECT 1;']);
  });
});
