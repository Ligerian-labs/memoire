import { expect, request, test, type Page } from '@playwright/test';
import { mkdirSync } from 'node:fs';
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

async function runSql(page: Page, sql: string): Promise<void> {
  await page.getByLabel('SQL').fill(sql);
  await page.getByRole('button', { name: /^Run/ }).click();
}

test('register, create a team and a database, write, read, browse rows, mint an API key', async ({
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
    await page.getByLabel('Email').fill(email);
    await page.getByLabel('Password').fill(password);
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
    await page.getByLabel('Name').fill(`Synthetic ${suffix}`);
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
    await dialog.getByLabel('Name').fill('scratch');
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

  await test.step('a write in read mode is refused', async () => {
    await page.getByRole('button', { name: 'Read', exact: true }).click();
    await runSql(page, `DELETE FROM notes`);
    await expect(page.getByTestId('console-error')).toContainText('write mode');
  });

  await test.step('read in the console', async () => {
    await page.getByRole('button', { name: 'Parameters' }).click();
    await page.getByLabel(/^Parameters \(JSON/).fill('["hello"]');
    await runSql(
      page,
      `SELECT id, body, score, data, 9007199254740993 AS big FROM notes WHERE body = ?`,
    );
    const grid = page.getByTestId('results').locator('table');
    await expect(grid.locator('tbody tr')).toHaveCount(1);
    await expect(grid.locator('tbody tr')).toContainText('hello');
    await expect(grid.locator('tbody tr')).toContainText('blob 2 B');
    await expect(grid.locator('tbody tr')).toContainText('9007199254740993');
    await page.getByLabel(/^Parameters \(JSON/).fill('');
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
    await machine.getByLabel('Name').fill('ci-runner');
    await machine.getByLabel('Description').fill('synthetic test machine');
    await machine.getByRole('button', { name: 'Create machine' }).click();
    const keyDialog = page.getByRole('dialog', { name: 'New API key' });
    await expect(keyDialog.getByLabel('Name')).toHaveValue('ci-runner');
    await keyDialog.getByLabel(/^Read-only/).check();
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
    await page.getByLabel('Name').fill(`Second ${suffix}`);
    await page.getByRole('button', { name: 'Create team' }).click();
    await expect(page.getByText('No databases in this team yet.')).toBeVisible();
    await page.getByTestId('team-switch').selectOption({ label: `Synthetic ${suffix}` });
    await expect(page.locator('tbody tr', { hasText: 'scratch' })).toBeVisible();
  });

  await test.step('sign out and back in', async () => {
    await page.getByRole('button', { name: 'Sign out' }).click();
    await expect(page.getByRole('heading', { name: 'Sign in' })).toBeVisible();
    await page.getByLabel('Email').fill(email);
    await page.getByLabel('Password').fill(password);
    await page.getByRole('button', { name: 'Sign in' }).click();
    await expect(page.locator('tbody tr', { hasText: 'scratch' })).toBeVisible();
  });

  expect(violations).toEqual([]);
});
