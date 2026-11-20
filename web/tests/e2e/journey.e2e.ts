import { expect, request, test, type Locator, type Page } from '@playwright/test';
import { mkdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

// One person's whole path through the debugger against a real gateway.
// Synthetic data only. Each step saves a screenshot for the PR.
const shots = join(process.cwd(), 'test-results/screenshots');
mkdirSync(shots, { recursive: true });
let shot = 0;
async function screenshot(page: Page, name: string): Promise<void> {
  shot += 1;
  await page.screenshot({ path: join(shots, `${String(shot).padStart(2, '0')}-${name}.png`) });
}

const suffix = Date.now().toString(36);
const email = `dev-${suffix}@example.test`;
const password = `synthetic-${suffix}-Pa55phrase`;

interface Capture {
  target: string;
  rendered: unknown;
}

/** The verification link the gateway captured instead of mailing it. */
async function verificationLink(page: Page): Promise<string> {
  let link: string | undefined;
  await expect
    .poll(
      async () => {
        const response = await page.request.get('/v1/notifications/dev/captures?channel=email');
        if (!response.ok()) return undefined;
        const captures = (await response.json()) as Capture[];
        const mine = captures.find((c) => c.target === email);
        link = mine
          ? /https?:\/\/[^\s"'<>\\]+#token=[A-Za-z0-9_\-.~%]+/.exec(
              JSON.stringify(mine.rendered),
            )?.[0]
          : undefined;
        return link;
      },
      { timeout: 30_000, message: 'the verification email is captured' },
    )
    .toBeTruthy();
  return link!;
}

/**
 * Fills a field and checks the value stayed. A route's DOM can be on screen
 * before Angular's first change detection (scheduled on a frame or a timer,
 * zoneless); that pass writes the form's initial value over an early fill. So
 * fill, let one frame and one timer pass, then check, and fill again if reset.
 */
async function fill(field: Locator, value: string): Promise<void> {
  await expect(async () => {
    await field.fill(value);
    await field
      .page()
      .evaluate(() => new Promise((done) => requestAnimationFrame(() => setTimeout(done, 50))));
    await expect(field).toHaveValue(value, { timeout: 500 });
  }).toPass({ timeout: 15_000 });
}

async function runSql(page: Page, sql: string): Promise<void> {
  await fill(page.getByLabel('SQL'), sql);
  await page.getByRole('button', { name: /^Run/ }).click();
}

test('register, create a team and databases, write, read, browse rows, restore a snapshot, mint an API key', async ({
  page,
  baseURL,
}) => {
  // A blocked script or stylesheet would leave the app half-styled without failing a locator.
  const violations: string[] = [];
  page.on('console', (message) => {
    if (message.type() === 'error' && /Content Security Policy|Refused to/.test(message.text())) {
      violations.push(message.text());
    }
  });

  await test.step('register and confirm the email', async () => {
    await page.goto('/');
    await expect(page.getByRole('heading', { name: 'Sign in' })).toBeVisible();
    await screenshot(page, 'sign-in');
    await page.getByRole('link', { name: 'Create one' }).click();
    // Both pages have an Email field: fill only once the register page is up.
    await expect(page.getByRole('heading', { name: 'Create an account' })).toBeVisible();
    await fill(page.getByLabel('Email'), email);
    await fill(page.getByLabel('Password'), password);
    await screenshot(page, 'register');
    await page.getByRole('button', { name: 'Create account' }).click();
    await expect(page.getByRole('heading', { name: 'Check your email' })).toBeVisible();
    const link = await verificationLink(page);
    expect(new URL(link).origin).toBe(new URL(baseURL!).origin);
    await page.goto(link);
    await expect(page.getByRole('heading', { name: 'New team' })).toBeVisible();
    expect(page.url()).not.toContain('token');
  });

  await test.step('create a team', async () => {
    await fill(page.getByLabel('Name'), `Synthetic ${suffix}`);
    await expect(page.getByLabel('Slug')).toHaveValue(`synthetic-${suffix}`);
    await screenshot(page, 'new-team');
    await page.getByRole('button', { name: 'Create team' }).click();
    await expect(page.getByRole('heading', { name: 'Databases' })).toBeVisible();
    await expect(page.getByTestId('team-switch')).toContainText(`Synthetic ${suffix}`);
    await expect(page.getByText('No databases in this team yet.')).toBeVisible();
  });

  await test.step('create a burstable database', async () => {
    await page.getByRole('button', { name: 'New database' }).click();
    const dialog = page.getByRole('dialog', { name: 'New database' });
    await fill(dialog.getByLabel('Name'), 'scratch');
    await dialog.getByLabel('Expires after idle for').selectOption({ label: '6 hours' });
    await screenshot(page, 'create-dialog');
    await dialog.getByRole('button', { name: 'Create database' }).click();
    await expect(page).toHaveURL(/\/databases\/scratch\/console$/);
    await expect(page.locator('.facts')).toContainText('6h');
  });

  await test.step('write in the console', async () => {
    await page.getByRole('button', { name: 'Write', exact: true }).click();
    await runSql(
      page,
      `CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL, score REAL, data BLOB);
CREATE INDEX notes_body ON notes (body);
INSERT INTO notes (body, score, data) VALUES ('hello', 1.5, x'0001'), ('world', NULL, NULL);
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 118)
INSERT INTO notes (body, score) SELECT 'note ' || i, i * 0.5 FROM n;`,
    );
    const results = page.getByTestId('results');
    await expect(results.locator('.result')).toHaveCount(4);
    await expect(results.locator('.result').nth(2)).toContainText('2 changed');
    await expect(results.locator('.result').nth(3)).toContainText('118 changed');
    await screenshot(page, 'console-write');
  });

  await test.step('a refused statement names itself, its SQLite code and message', async () => {
    await runSql(
      page,
      `INSERT INTO notes (body) VALUES ('ok');\nINSERT INTO notes (body) VALUES (NULL);`,
    );
    const error = page.getByTestId('console-error');
    await expect(error).toContainText('Statement 2');
    await expect(error).toContainText('NOT NULL constraint failed: notes.body');
    await expect(error).toContainText('SQLITE_CONSTRAINT');
    await expect(error.locator('code')).toHaveText('INSERT INTO notes (body) VALUES (NULL);');
    // Without a transaction the statement before the failing one stays.
    await runSql(page, `DELETE FROM notes WHERE body = 'ok'`);
    await expect(page.getByTestId('results').locator('.result').first()).toContainText('1 changed');
  });

  await test.step('a write in read mode is refused', async () => {
    await page.getByRole('button', { name: 'Read', exact: true }).click();
    await runSql(page, `DELETE FROM notes`);
    await expect(page.getByTestId('console-error')).toContainText('write mode');
  });

  await test.step('read in the console', async () => {
    await page.getByRole('button', { name: 'Parameters' }).click();
    await fill(page.getByRole('textbox', { name: /Parameters/ }), '["hello"]');
    await runSql(
      page,
      `SELECT id, body, score, data, 9007199254740993 AS big FROM notes WHERE body = ?`,
    );
    const grid = page.getByTestId('results').locator('table');
    await expect(grid.locator('tbody tr')).toHaveCount(1);
    await expect(grid.locator('tbody tr')).toContainText('hello');
    await expect(grid.locator('tbody tr')).toContainText('blob 2 B');
    await expect(grid.locator('tbody tr')).toContainText('9007199254740993');
    await fill(page.getByRole('textbox', { name: /Parameters/ }), '');
    await runSql(page, `SELECT body, score FROM notes ORDER BY id LIMIT 5`);
    await expect(page.getByTestId('results').locator('tbody tr')).toHaveCount(5);
    await screenshot(page, 'console-read');
  });

  await test.step('browse rows page by page', async () => {
    await page
      .getByRole('complementary', { name: 'Tables and views' })
      .getByRole('link', { name: 'notes' })
      .click();
    await expect(page).toHaveURL(/\/tables\/notes$/);
    await page.getByLabel('Rows per page').selectOption('50');
    await expect(page.getByText('Rows 1–50')).toBeVisible();
    await page.getByRole('button', { name: 'Next' }).click();
    await expect(page.getByText('Rows 51–100')).toBeVisible();
    await expect(page.locator('table.grid tbody tr').first()).toContainText('note 49');
    await screenshot(page, 'rows');
    await page.getByRole('button', { name: 'Next' }).click();
    await expect(page.getByText('Rows 101–120')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Next' })).toBeDisabled();
    await page.getByRole('button', { name: 'Previous' }).click();
    await expect(page.getByText('Rows 51–100')).toBeVisible();
  });

  await test.step('read the schema', async () => {
    await page.getByRole('link', { name: 'Schema' }).click();
    await expect(page.locator('app-schema')).toContainText('notes_body');
    await expect(page.locator('app-schema')).toContainText('NOT NULL');
    await screenshot(page, 'schema');
    await page.getByRole('link', { name: 'Snapshots' }).click();
    await expect(page.getByText('Burstable databases have no snapshots.')).toBeVisible();
  });

  await test.step('a memory store is grouped apart from the application tables', async () => {
    await page.getByRole('link', { name: 'Console' }).click();
    await page.getByRole('button', { name: 'Write', exact: true }).click();
    await runSql(
      page,
      `CREATE TABLE memoire_memory_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE memoire_memory_memories (seq INTEGER PRIMARY KEY, id TEXT NOT NULL, title TEXT NOT NULL);
CREATE VIRTUAL TABLE memoire_memory_fts USING fts5(title, body);`,
    );
    await expect(page.getByTestId('results').locator('.result')).toHaveCount(3);
    const objects = page.getByRole('complementary', { name: 'Tables and views' });
    const tables = objects.locator('h3:text-is("Tables") + ul');
    await expect(tables.getByRole('link')).toHaveText(['notes']);
    const store = objects.locator('.store');
    await expect(store.getByRole('heading', { name: 'Memory store' })).toBeVisible();
    await expect(store.getByRole('link', { name: 'memoire_memory_memories' })).toBeVisible();
    await expect(
      store.getByRole('link', { name: 'memoire_memory_fts', exact: true }),
    ).toBeVisible();
    const internal = store.getByRole('link', { name: 'memoire_memory_fts_data' });
    await expect(internal).toBeHidden();
    await screenshot(page, 'memory-store-list');
    await store.getByText('Search index internals (5)').click();
    await expect(internal).toBeVisible();

    await page.getByRole('link', { name: 'Schema' }).click();
    const schema = page.locator('app-schema');
    await expect(schema.getByRole('heading', { name: 'Memory store' })).toBeVisible();
    const badge = schema.locator('.badge', { hasText: 'FTS5 internal' }).first();
    await expect(badge).toBeHidden();
    await schema.getByText('Search index internals (5)').click();
    await expect(badge).toBeVisible();
    await expect(schema.locator('.badge', { hasText: 'FTS5 internal' })).toHaveCount(5);
    await schema
      .locator('details.internals')
      .evaluate((el) => el.scrollIntoView({ block: 'start' }));
    await screenshot(page, 'memory-store-schema');
  });

  await test.step('snapshot a persistent database, restore it and export it', async () => {
    await page
      .getByRole('navigation', { name: 'Main' })
      .getByRole('link', { name: 'Databases' })
      .click();
    await page.getByRole('button', { name: 'New database' }).click();
    const dialog = page.getByRole('dialog', { name: 'New database' });
    await fill(dialog.getByLabel('Name'), 'keeper');
    await dialog.getByRole('radio', { name: /Persistent/ }).check();
    await dialog.getByRole('button', { name: 'Create database' }).click();
    await expect(page).toHaveURL(/\/databases\/keeper\/console$/);
    await page.getByRole('button', { name: 'Write', exact: true }).click();
    await runSql(
      page,
      `CREATE TABLE kv (k TEXT PRIMARY KEY, v TEXT);\nINSERT INTO kv VALUES ('a', 'v1');`,
    );
    await expect(page.getByTestId('results').locator('.result')).toHaveCount(2);
    const written = Date.now();

    // Wait for a scheduled snapshot taken after the write (shown to the second, so strictly later).
    await page.getByRole('link', { name: 'Snapshots' }).click();
    const snapshots = page.locator('app-snapshots');
    let id = '';
    await expect(async () => {
      await snapshots.getByRole('button', { name: 'Refresh' }).click();
      const newest = snapshots.locator('tbody tr').first();
      const taken = Date.parse(
        ((await newest.locator('td').first().textContent()) ?? '').trim().replace(' ', 'T'),
      );
      expect(taken).toBeGreaterThan(written);
      id = ((await newest.locator('td').nth(3).textContent()) ?? '').trim();
      expect(id).toMatch(/^snap_/);
    }).toPass({ timeout: 30_000 });

    await page.getByRole('link', { name: 'Console' }).click();
    await runSql(page, `UPDATE kv SET v = 'v2'`);
    await expect(page.getByTestId('results').locator('.result')).toHaveCount(1);
    await expect(page.getByTestId('results')).toContainText('1 changed');

    await page.getByRole('link', { name: 'Snapshots' }).click();
    await snapshots
      .locator('tbody tr', { hasText: id })
      .getByRole('button', { name: 'Restore' })
      .click();
    const confirm = page.getByRole('dialog', { name: 'Restore keeper' });
    await screenshot(page, 'restore-dialog');
    await confirm.getByRole('button', { name: 'Restore snapshot' }).click();
    await expect(snapshots.getByRole('status')).toContainText('Restored the snapshot');
    await expect(snapshots.locator('tbody')).toContainText('before a restore');
    await screenshot(page, 'snapshots');

    await page.getByRole('link', { name: 'Console' }).click();
    await page.getByRole('button', { name: 'Read', exact: true }).click();
    await runSql(page, `SELECT v FROM kv`);
    await expect(page.getByTestId('results').locator('tbody tr')).toHaveText(/v1/);

    const download = page.waitForEvent('download');
    await page.getByRole('button', { name: 'Export' }).click();
    const file = await download;
    expect(file.suggestedFilename()).toBe('keeper.sqlite');
    const bytes = readFileSync(await file.path());
    expect(bytes.subarray(0, 16).toString('latin1')).toBe('SQLite format 3\u0000');
  });

  await test.step('list the databases', async () => {
    await page
      .getByRole('navigation', { name: 'Main' })
      .getByRole('link', { name: 'Databases' })
      .click();
    const row = page.locator('tbody tr', { hasText: 'scratch' });
    await expect(row).toContainText('burstable');
    await expect(row).toContainText('active');
    await expect(row).toContainText(/in (5h|6h)/);
    await screenshot(page, 'databases');
  });

  let key = '';
  await test.step('create a machine and mint its API key', async () => {
    await page.getByRole('link', { name: 'Machines and API keys' }).click();
    await page.getByRole('button', { name: 'New machine' }).click();
    const machine = page.getByRole('dialog', { name: 'New machine' });
    await fill(machine.getByLabel('Name'), 'ci-runner');
    await fill(machine.getByLabel('Description'), 'synthetic test machine');
    await machine.getByRole('button', { name: 'Create machine' }).click();
    const keyDialog = page.getByRole('dialog', { name: 'New API key' });
    await expect(keyDialog.getByLabel('Name')).toHaveValue('ci-runner');
    await keyDialog.getByRole('radio', { name: /Read-only/ }).check();
    await keyDialog.getByRole('button', { name: 'Create key' }).click();
    key = (await page.getByTestId('minted-key').textContent())?.trim() ?? '';
    expect(key).toMatch(/^mzk_/);
    await page
      .getByTestId('minted-key')
      .evaluate((el) => (el.textContent = 'mzk_••••••••••••••••'));
    await screenshot(page, 'key-created');
    await page.getByRole('button', { name: 'Done' }).click();
    const row = page.locator('tbody tr', { hasText: 'ci-runner' }).last();
    await expect(row).toContainText('read-only');
    await screenshot(page, 'machines');
  });

  await test.step('the key works, read-only, in its team', async () => {
    const agent = await request.newContext({
      baseURL,
      extraHTTPHeaders: { Authorization: `Bearer ${key}` },
    });
    const whoami = await agent.get('/v1/memoire/whoami');
    expect(whoami.status()).toBe(200);
    expect(await whoami.json()).toMatchObject({ kind: 'api_key' });
    const read = await agent.post('/v1/memoire/databases/scratch/query', {
      data: { mode: 'read', statements: [{ sql: 'SELECT count(*) AS n FROM notes' }] },
    });
    expect(read.status()).toBe(200);
    expect((await read.json()).results[0].rows).toEqual([[120]]);
    const write = await agent.post('/v1/memoire/databases/scratch/query', {
      data: { mode: 'write', statements: [{ sql: 'DELETE FROM notes' }] },
    });
    expect(write.status()).toBe(403);
    await agent.dispose();
  });

  await test.step('switch teams', async () => {
    await page.getByRole('link', { name: 'New team' }).click();
    await expect(page.getByRole('heading', { name: 'New team' })).toBeVisible();
    await fill(page.getByLabel('Name'), `Second ${suffix}`);
    await page.getByRole('button', { name: 'Create team' }).click();
    await expect(page.getByText('No databases in this team yet.')).toBeVisible();
    await page.getByTestId('team-switch').selectOption({ label: `Synthetic ${suffix}` });
    await expect(page.locator('tbody tr', { hasText: 'scratch' })).toBeVisible();
  });

  await test.step('sign out and back in', async () => {
    await page.getByRole('button', { name: 'Sign out' }).click();
    await expect(page.getByRole('heading', { name: 'Sign in' })).toBeVisible();
    await fill(page.getByLabel('Email'), email);
    await fill(page.getByLabel('Password'), password);
    await page.getByRole('button', { name: 'Sign in' }).click();
    await expect(page.locator('tbody tr', { hasText: 'scratch' })).toBeVisible();
  });

  expect(violations).toEqual([]);
});
