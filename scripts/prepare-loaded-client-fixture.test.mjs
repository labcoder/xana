import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { test } from 'node:test';
import path from 'node:path';
import { classifyRequest, runCli, seedLine, startFixtureServer, validFixtureLeaf } from './prepare-loaded-client-fixture.mjs';

const body = content => JSON.stringify({ model: 'synthetic-loaded-client', stream: true, messages: [{ role: 'user', content }] });

test('fixture prompts and paths remain narrow', () => {
  assert.equal(classifyRequest(body(seedLine(319))), 'seed');
  assert.equal(classifyRequest(body('fixture:slow')), 'slow');
  assert.equal(classifyRequest(body('fixture:next')), 'next');
  assert.throws(() => classifyRequest(body('ordinary user content')));
  assert.throws(() => classifyRequest('{'));
  assert.throws(() => classifyRequest(JSON.stringify({ messages: [] })));
  assert.equal(validFixtureLeaf('loaded-01234567-0123-0123-0123-0123456789ab'), true);
  for (const value of ['..', 'loaded-../home', 'home', 'loaded-']) assert.equal(validFixtureLeaf(value), false);
});

test('loopback server bounds routes, body size and synthetic output', async () => {
  const fixture = await startFixtureServer({ token: randomBytes(16).toString('hex') });
  try {
    assert.match(fixture.url, /^http:\/\/127\.0\.0\.1:\d+\/[a-f0-9]{32}\/v1$/);
    assert.equal((await fetch(fixture.url + '/models')).status, 200);
    assert.equal((await fetch(fixture.url + '/not-a-route')).status, 404);
    assert.equal((await fetch(fixture.url + '/chat/completions', { method: 'POST', body: body('real content') })).status, 400);
    assert.equal((await fetch(fixture.url + '/chat/completions', { method: 'POST', body: 'x'.repeat(512 * 1024 + 1) })).status, 413);
    const response = await fetch(fixture.url + '/chat/completions', { method: 'POST', body: body(seedLine(0)) });
    const text = await response.text();
    assert.equal(response.status, 200);
    assert.match(text, /Synthetic seed result/);
    assert.match(text, /data: \[DONE\]/);
    assert.equal(fixture.stats.completed, 1);
  } finally { await fixture.close(); }
});

test('cancelled slow stream releases its slot and the next request completes', async () => {
  const fixture = await startFixtureServer({ token: randomBytes(16).toString('hex'), frameDelay: 2 });
  try {
    const abort = new AbortController();
    const response = await fetch(fixture.url + '/chat/completions', { method: 'POST', body: body('fixture:slow'), signal: abort.signal });
    const reader = response.body.getReader();
    assert.equal((await reader.read()).done, false);
    abort.abort();
    await assert.rejects(reader.read());
    const next = await fetch(fixture.url + '/chat/completions', { method: 'POST', body: body('fixture:next') });
    assert.match(await next.text(), /Synthetic next result/);
    assert.equal(fixture.stats.completed, 1);
    assert.ok(fixture.stats.frames < 241);
  } finally { await fixture.close(); }
});

test('server restarts on the same route without owning any durable state', async () => {
  const token = randomBytes(16).toString('hex');
  const first = await startFixtureServer({ token });
  const port = first.port;
  await first.close();
  const second = await startFixtureServer({ token, port });
  try {
    const response = await fetch(second.url + '/chat/completions', { method: 'POST', body: body('fixture:next') });
    assert.match(await response.text(), /Synthetic next result/);
  } finally { await second.close(); }
});

test('failed spawn settles blocked stdin without an unhandled rejection', async () => {
  const fixture = { binary: path.join(process.cwd(), 'target', 'missing-' + randomBytes(16).toString('hex')), workspace: process.cwd(), home: 'unused-synthetic-home', key: 'unused-synthetic-key' };
  await assert.rejects(runCli(fixture, [], Array.from({ length: 320 }, (_, index) => seedLine(index))), /cli_spawn_failed/);
});

test('early stdin close and nonzero exit settle backpressure and child lifecycle', async () => {
  const fixture = { binary: process.execPath, workspace: process.cwd(), home: 'unused-synthetic-home', key: 'unused-synthetic-key' };
  await assert.rejects(runCli(fixture, ['-e', 'process.stdin.destroy(); setTimeout(() => process.exit(7), 5)'], Array.from({ length: 64 }, () => 'x'.repeat(16 * 1024))), /EPIPE|cli_failed_code_7/);
});

test('stdout decoding preserves multilingual bytes split across chunks', async () => {
  const fixture = { binary: process.execPath, workspace: process.cwd(), home: 'unused-synthetic-home', key: 'unused-synthetic-key' };
  const expected = JSON.stringify({ text: '日本語 مرحبا français λ 🦀' });
  const program = `const bytes = Buffer.from(${JSON.stringify(expected)}); let i = 0; const timer = setInterval(() => { if (i === bytes.length) return clearInterval(timer); process.stdout.write(bytes.subarray(i, ++i)); }, 1);`;
  assert.equal(await runCli(fixture, ['-e', program]), expected);
});

test('CLI timeout closes the owned process', async () => {
  const fixture = { binary: process.execPath, workspace: process.cwd(), home: 'unused-synthetic-home', key: 'unused-synthetic-key' };
  await assert.rejects(runCli(fixture, ['-e', 'setInterval(() => {}, 1000)'], [], 30), /cli_time_or_output_bound/);
});
