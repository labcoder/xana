#!/usr/bin/env node
// Development-only synthetic fixture. Never imports a journal or opens a real home.
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { createReadStream } from 'node:fs';
import { lstat, mkdir, readFile, realpath, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';

const repository = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const fixtureParent = path.join(repository, 'target', 'loaded-client-fixtures');
const REQUEST_BYTES = 512 * 1024;
const OUTPUT_BYTES = 4 * 1024 * 1024;
const MAX_REQUESTS = 1000;
const SEED_TURNS = 320;
const children = new Set();
let activeServer;
let stopping = false;

function fail(code) { throw new Error(code); }
function check(condition, code) { if (!condition) fail(code); }
export function seedLine(index) {
  return `fixture:seed:${index} Synthetic evidence 日本語 مرحبا français λ 🦀. Keep entry ${index} distinct.`;
}

export function classifyRequest(body) {
  let request;
  try { request = JSON.parse(body); } catch { fail('invalid_json'); }
  check(Array.isArray(request.messages) && request.messages.length <= 1024, 'invalid_messages');
  check(request.model === 'synthetic-loaded-client' && request.stream === true, 'invalid_route');
  const last = request.messages.findLast(message => message.role === 'user');
  const content = last?.content;
  check(typeof content === 'string' && Buffer.byteLength(content) <= 16 * 1024, 'invalid_user_content');
  if (/^fixture:seed:\d{1,3} Synthetic evidence /u.test(content)) return 'seed';
  if (content === 'fixture:slow') return 'slow';
  if (content === 'fixture:next') return 'next';
  fail('synthetic_prompt_required');
}

export async function startFixtureServer({ token, port = 0, frameDelay = 100 }) {
  check(/^[a-f0-9]{32}$/.test(token), 'invalid_fixture_token');
  check(Number.isInteger(port) && port >= 0 && port <= 65535, 'invalid_port');
  const sockets = new Set();
  const timers = new Set();
  const stats = { requests: 0, completed: 0, cancelled: 0, rejected: 0, frames: 0, active: 0 };
  const prefix = `/${token}/v1`;
  const server = createServer({ maxHeaderSize: 8192 }, async (request, response) => {
    const reject = status => { stats.rejected++; response.writeHead(status).end(); };
    if (++stats.requests > MAX_REQUESTS || stats.active >= 4) return reject(429);
    if (request.method === 'GET' && request.url === `${prefix}/models`) {
      response.writeHead(200, { 'Content-Type': 'application/json', Connection: 'close' });
      response.end(JSON.stringify({ data: [{ id: 'synthetic-loaded-client' }] }));
      return;
    }
    if (request.method !== 'POST' || request.url !== `${prefix}/chat/completions`) return reject(404);
    if (Number(request.headers['content-length']) > REQUEST_BYTES) return reject(413);
    stats.active++;
    let finished = false;
    let timer;
    const deadline = setTimeout(() => response.destroy(), 35_000);
    timers.add(deadline);
    response.once('close', () => {
      clearTimeout(deadline); timers.delete(deadline);
      clearTimeout(timer); timers.delete(timer);
      stats.active--;
      if (!finished && response.headersSent) stats.cancelled++;
    });
    try {
      const chunks = [];
      let bytes = 0;
      for await (const chunk of request) {
        bytes += chunk.length;
        if (bytes > REQUEST_BYTES) { reject(413); return; }
        chunks.push(chunk);
      }
      const kind = classifyRequest(Buffer.concat(chunks).toString('utf8'));
      response.writeHead(200, { 'Content-Type': 'text/event-stream', Connection: 'close', 'Cache-Control': 'no-store' });
      response.flushHeaders();
      const count = kind === 'slow' ? 240 : 1;
      let index = 0;
      const send = () => {
        timers.delete(timer);
        if (response.destroyed) return;
        const text = kind === 'slow'
          ? `Synthetic stream ${index + 1}/240 日本語 🦀. `
          : `Synthetic ${kind} result: 日本語 مرحبا français λ 🦀.\n`;
        const frame = `data: ${JSON.stringify({ choices: [{ delta: { content: text } }] })}\n\n`;
        check(Buffer.byteLength(frame) <= 2048, 'frame_bound');
        stats.frames++;
        index++;
        const drained = response.write(frame);
        const next = () => {
          if (response.destroyed) return;
          if (index === count) {
            finished = true; stats.completed++;
            response.end('data: [DONE]\n\n');
          } else {
            timer = setTimeout(send, frameDelay); timers.add(timer);
          }
        };
        if (drained) next(); else response.once('drain', next);
      };
      send();
    } catch {
      if (!response.headersSent) reject(400); else response.destroy();
    }
  });
  server.headersTimeout = 5000;
  server.requestTimeout = 10_000;
  server.keepAliveTimeout = 1000;
  server.maxConnections = 8;
  server.on('connection', socket => {
    sockets.add(socket);
    socket.setTimeout(35_000, () => socket.destroy());
    socket.once('close', () => sockets.delete(socket));
  });
  server.on('clientError', (_, socket) => socket.destroy());
  server.listen(port, '127.0.0.1');
  await once(server, 'listening');
  const selectedPort = server.address().port;
  return {
    port: selectedPort, url: `http://127.0.0.1:${selectedPort}${prefix}`, stats,
    async close() {
      for (const timer of timers) clearTimeout(timer);
      timers.clear();
      for (const socket of sockets) socket.destroy();
      if (server.listening) await new Promise(resolve => server.close(resolve));
    },
  };
}

async function safeParent() {
  const canonicalRepository = await realpath(repository);
  for (const directory of [path.join(repository, 'target'), fixtureParent]) {
    await mkdir(directory, { recursive: true });
    const info = await lstat(directory);
    check(info.isDirectory() && !info.isSymbolicLink(), 'fixture_parent_must_be_directory');
    check((await realpath(directory)).startsWith(canonicalRepository + path.sep), 'fixture_parent_outside_checkout');
  }
}

export function validFixtureLeaf(name) { return /^loaded-[a-f0-9-]{36}$/.test(name); }

async function loadFixture(directory) {
  await safeParent();
  const candidate = path.resolve(directory);
  check(path.dirname(candidate) === fixtureParent && validFixtureLeaf(path.basename(candidate)), 'fixture_path_outside_owned_parent');
  check(!(await lstat(candidate)).isSymbolicLink() && (await realpath(candidate)) === candidate, 'fixture_link_rejected');
  const file = path.join(candidate, 'fixture.json');
  check((await lstat(file)).size <= 16 * 1024 && !(await lstat(file)).isSymbolicLink(), 'manifest_bound');
  const fixture = JSON.parse(await readFile(file, 'utf8'));
  check(fixture.schema === 1 && fixture.synthetic_only === true && fixture.root === candidate, 'invalid_manifest');
  check(fixture.home === path.join(candidate, 'home') && fixture.key === path.join(candidate, 'recovery.key') && fixture.workspace === path.join(candidate, 'workspace'), 'invalid_fixture_paths');
  for (const entry of [fixture.home, fixture.workspace, fixture.key]) check(!(await lstat(entry)).isSymbolicLink(), 'fixture_link_rejected');
  // Fixed managed paths only; do not scan unrelated directories or the user's home.
  for (const name of ['data', 'data/protected']) {
    const entry = path.join(fixture.home, name);
    check(!(await lstat(entry)).isSymbolicLink() && (await realpath(entry)).startsWith(fixture.home + path.sep), 'fixture_managed_path_link_rejected');
  }
  check(/^[a-f0-9-]{36}$/.test(fixture.session), 'invalid_session');
  return fixture;
}

export async function runCli(fixture, args, lines = [], timeout = 30_000) {
  check(!stopping, 'fixture_stopping');
  const child = spawn(fixture.binary, args, {
    cwd: fixture.workspace, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'],
    env: { ...process.env, XANA_HOME: fixture.home, XANA_STORAGE_RECOVERY_KEY: fixture.key, NO_COLOR: '1' },
  });
  children.add(child);
  let stdout = ''; let stdoutBytes = 0; let stderrBytes = 0; let exceeded = false;
  const timer = setTimeout(() => { exceeded = true; child.kill(); }, timeout);
  const forceTimer = setTimeout(() => child.kill('SIGKILL'), timeout + 2000);
  child.stdout.setEncoding('utf8');
  child.stdout.on('data', chunk => {
    stdoutBytes += Buffer.byteLength(chunk);
    if (stdoutBytes > OUTPUT_BYTES) { exceeded = true; child.kill(); } else stdout += chunk;
  });
  child.stderr.on('data', chunk => {
    stderrBytes += chunk.length;
    if (stderrBytes > 128 * 1024) { exceeded = true; child.kill(); }
  });
  // Attach the exit/error observer before producing stdin; never await a pipe with no reader.
  const closed = new Promise((resolve, reject) => {
    child.once('error', () => reject(new Error('cli_spawn_failed')));
    child.once('close', (code, signal) => resolve({ code, signal }));
  });
  child.stdin.on('error', () => {});
  const producer = (async () => {
    for (const line of lines) {
      if (!child.stdin.write(line + '\n', 'utf8')) {
        const ready = await Promise.race([once(child.stdin, 'drain').then(() => true), closed.then(() => false)]);
        if (!ready) break;
      }
    }
    child.stdin.end();
  })();
  try {
    const [result] = await Promise.all([closed, producer]);
    check(!exceeded, 'cli_time_or_output_bound');
    check(result.code === 0, `cli_failed_code_${result.code ?? 'signal'}_stderr_bytes_${stderrBytes}`);
    return stdout;
  } finally {
    clearTimeout(timer);
    if (child.pid && child.exitCode === null && child.signalCode === null) child.kill();
    await closed.catch(() => {});
    clearTimeout(forceTimer);
    children.delete(child);
  }
}

async function binaryDigest(binary) {
  const info = await lstat(binary);
  check(info.isFile() && info.size <= 1024 * 1024 * 1024, 'binary_must_be_bounded_regular_file');
  const digest = createHash('sha256');
  for await (const chunk of createReadStream(binary, { highWaterMark: 64 * 1024 })) digest.update(chunk);
  return digest.digest('hex');
}

function countEntries(inspection) {
  const match = inspection.match(/^active history entries: (\d+)$/m);
  check(match, 'missing_history_count');
  return Number(match[1]);
}

function validatePreview(output, minimum) {
  const preview = JSON.parse(output);
  check(preview.total >= minimum && preview.has_older === true, 'preview_missing_older_history');
  check(Array.isArray(preview.messages) && preview.messages.length <= 32 && preview.messages.length > 0, 'preview_window_bound');
  check(preview.start === preview.total - preview.messages.length, 'preview_range_mismatch');
  return { total: preview.total, start: preview.start, retained_messages: preview.messages.length };
}

async function prepare(binary) {
  await safeParent();
  const root = path.join(fixtureParent, `loaded-${randomUUID()}`);
  await mkdir(root, { mode: 0o700 });
  const fixture = {
    schema: 1, synthetic_only: true, root, home: path.join(root, 'home'),
    workspace: path.join(root, 'workspace'), key: path.join(root, 'recovery.key'),
    binary: await realpath(binary), token: randomBytes(16).toString('hex'),
    created_utc: new Date().toISOString(), seed_turns: SEED_TURNS,
  };
  await mkdir(fixture.workspace);
  fixture.binary_sha256 = await binaryDigest(fixture.binary);
  process.stdout.write(JSON.stringify({ stage: 'created', fixture: root }) + '\n');
  activeServer = await startFixtureServer({ token: fixture.token });
  fixture.port = activeServer.port;
  await runCli(fixture, ['storage', 'recovery-key', '--output', fixture.key]);
  await runCli(fixture, ['storage', 'initialize', '--manual-unlock', '--recovery-key', fixture.key]);
  await runCli(fixture, ['init', '--non-interactive', '--kind', 'ollama', '--provider-name', 'fixture', '--base-url', activeServer.url, '--model', 'synthetic-loaded-client', '--permission-mode', 'deny']);
  const output = await runCli(fixture, ['--plain', '--no-banner'], [
    ...Array.from({ length: SEED_TURNS }, (_, index) => seedLine(index)), '/compact', '/quit',
  ], 180_000);
  fixture.session = output.match(/^session: ([a-f0-9-]{36})$/m)?.[1];
  check(fixture.session, 'seed_session_not_reported');
  check(activeServer.stats.completed === SEED_TURNS, 'seed_did_not_complete_all_turns');
  const inspection = await runCli(fixture, ['session', 'inspect', fixture.session]);
  fixture.seed_entries = countEntries(inspection);
  check(fixture.seed_entries >= 640, 'seed_below_640_messages');
  fixture.compactions = Number(inspection.match(/^compactions: (\d+)$/m)?.[1] ?? 0);
  check(fixture.compactions > 0, 'seed_compaction_not_committed');
  const oldStats = { ...activeServer.stats };
  await activeServer.close();
  activeServer = await startFixtureServer({ token: fixture.token, port: fixture.port });
  await runCli(fixture, ['--resume', fixture.session, '--print', 'fixture:next', '--output', 'json']);
  fixture.durable_entries = countEntries(await runCli(fixture, ['session', 'inspect', fixture.session]));
  check(fixture.durable_entries >= fixture.seed_entries + 2 && activeServer.stats.completed === 1, 'restart_resume_did_not_append');
  const slow = runCli(fixture, ['--resume', fixture.session, '--print', 'fixture:slow', '--output', 'stream-json'], [], 40_000);
  const previewDuringStream = (async () => {
    const deadline = performance.now() + 5000;
    while (activeServer.stats.active === 0 && performance.now() < deadline) await delay(20);
    check(activeServer.stats.active === 1, 'slow_stream_did_not_start');
    const preview = validatePreview(await runCli(fixture, ['session', 'preview', fixture.session, '--limit', '32', '--json']), fixture.durable_entries);
    check(activeServer.stats.active === 1, 'preview_did_not_overlap_stream');
    return preview;
  })();
  const [streamOutput, streamPreview] = await Promise.all([slow, previewDuringStream]);
  const frames = streamOutput.trim().split('\n').map(line => JSON.parse(line));
  check(frames.length >= 2 && frames.length <= 1024 && frames.every((frame, index) => frame.sequence === index + 1), 'stream_sequence_bound');
  check(frames.at(-1).type === 'result' && frames.at(-1).payload.status === 'success', 'stream_result_failed');
  check(activeServer.stats.frames === 241 && activeServer.stats.completed === 2, 'slow_stream_not_complete');
  fixture.slow_stream = { provider_frames: 240, cli_frames: frames.length, preview_during_stream: streamPreview, cancellation_client_verified: false };
  fixture.durable_entries = countEntries(await runCli(fixture, ['session', 'inspect', fixture.session]));
  check(fixture.durable_entries >= fixture.seed_entries + 4, 'stream_did_not_append');
  await runCli(fixture, ['storage', 'verify'], [], 60_000);
  fixture.server_restart_resume = true;
  fixture.seed_server = oldStats;
  fixture.native_controls_measured = false;
  await writeFile(path.join(root, 'fixture.json'), JSON.stringify(fixture, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
  process.stdout.write(JSON.stringify({ stage: 'prepared', fixture: root, session: fixture.session, durable_entries: fixture.durable_entries, server_restart_resume: true, native_controls_measured: false }) + '\n');
}

async function measure(fixture) {
  check(await binaryDigest(fixture.binary) === fixture.binary_sha256, 'binary_changed_prepare_again');
  const samples = [];
  for (let index = 0; index < 20; index++) {
    const started = performance.now();
    const preview = await runCli(fixture, ['session', 'preview', fixture.session, '--limit', '32', '--json']);
    check(Buffer.byteLength(preview) <= 128 * 1024, 'preview_bound');
    validatePreview(preview, fixture.durable_entries);
    samples.push(performance.now() - started);
  }
  samples.sort((a, b) => a - b);
  const evidence = { schema: 1, scenario: 'fresh-cli-process-bounded-preview-not-native-controls', recorded_utc: new Date().toISOString(), binary_sha256: fixture.binary_sha256, samples: samples.length, p50_ms: samples[9], p95_ms: samples[18], native_input_fps_or_concurrent_maintenance_measured: false };
  await writeFile(path.join(fixture.root, `preview-${randomUUID()}.json`), JSON.stringify(evidence, null, 2) + '\n', { flag: 'wx' });
  process.stdout.write(JSON.stringify(evidence) + '\n');
}

async function shutdown() {
  if (stopping) return;
  stopping = true;
  const closed = [...children].map(child => {
    const ended = once(child, 'close').catch(() => {});
    child.kill();
    const force = setTimeout(() => child.kill('SIGKILL'), 2000);
    return ended.finally(() => clearTimeout(force));
  });
  if (activeServer) await activeServer.close();
  await Promise.all(closed);
}

async function main() {
  const [mode, value, ...rest] = process.argv.slice(2);
  check(rest.length === 0 && value && ['--prepare', '--serve', '--measure'].includes(mode), 'usage_node_script_--prepare_BINARY_or_--serve_FIXTURE_or_--measure_FIXTURE');
  process.once('SIGINT', () => { void shutdown(); });
  process.once('SIGTERM', () => { void shutdown(); });
  try {
    if (mode === '--prepare') await prepare(value);
    else {
      const fixture = await loadFixture(value);
      if (mode === '--measure') await measure(fixture);
      else {
        activeServer = await startFixtureServer({ token: fixture.token, port: fixture.port });
        process.stdout.write(JSON.stringify({ stage: 'serving', fixture: fixture.root, session: fixture.session, loopback_port: fixture.port, deadline_minutes: 60 }) + '\n');
        // Lifetime-bound: a foreground owner stops this server with Ctrl+C; unattended expiry is one hour.
        const deadline = setTimeout(() => { void shutdown(); }, 60 * 60 * 1000);
        while (!stopping) await delay(250);
        clearTimeout(deadline);
        process.stdout.write(JSON.stringify({ stage: 'stopped', ...activeServer.stats }) + '\n');
      }
    }
  } finally { await shutdown(); }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(error => {
    const safe = /^[a-zA-Z0-9_-]+$/.test(error.message) ? error.message : 'fixture_operation_failed';
    process.stderr.write(JSON.stringify({ error: safe, content_logged: false }) + '\n');
    process.exitCode = 1;
  });
}
