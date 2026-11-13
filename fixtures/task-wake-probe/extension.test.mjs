import test from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { createHash } from 'node:crypto';
import child from 'node:child_process';
import { syncBuiltinESMExports } from 'node:module';
import extension, { configuration, validatePacket, deliver } from '../../scripts/task-wake-extension.ts';

const id = 'sha256:' + 'a'.repeat(64);
const packet = { schema: 'af/task-wake-pi-next@1', session: 'original', delivery_id: id,
  message: 'AF Task wake metadata. Task: example; Store: ' + id + '; receipt: ' + id + ". Inspect current Task evidence and report it under this session's existing authority. This observation may be historical, superseded or source-collected. Do not infer current completion from this wake. This wake authorizes no Task execution, resume, login, budget increase or publication.",
  admission: 'unknown_if_packet_returned', turn_started: null, reported: null };
function context(session = 'original', idle = true, mode = 'interactive') {
  return { sessionManager: { getSessionId: () => session }, isIdle: () => idle, mode,
    ui: { notify() {} } };
}
function config(t) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'af-production-wake-')));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.chmodSync(root, 0o700);
  const route = path.join(root, 'route.json');
  fs.writeFileSync(route, '{}', { mode: 0o600 });
  const af = path.join(root, 'installed-af');
  fs.writeFileSync(af, '#!/bin/sh\nexit 1\n', { mode: 0o700 });
  const value = { schema: 'af/task-wake-pi-config@1', af,
    af_digest: 'sha256:' + createHash('sha256').update(fs.readFileSync(af)).digest('hex'),
    repo: root, state: root, route };
  const file = path.join(root, 'config.json');
  fs.writeFileSync(file, JSON.stringify(value), { mode: 0o600 });
  return { root, file, value };
}
test('production bridge uses original interactive session and followUp only', () => {
  const calls = [];
  deliver({ sendMessage: (...a) => calls.push(a) }, context(), packet);
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0][1], { triggerTurn: true, deliverAs: 'followUp' });
  assert.equal(calls[0][0].details.delivery_id, id);
  for (const ctx of [context('wrong'), context('original', true, 'rpc')]) {
    assert.throws(() => deliver({ sendMessage() { assert.fail('native authority widened'); } }, ctx, packet));
  }
});
test('closed bounded response never invents turn or report evidence', () => {
  assert.equal(validatePacket(packet, 'original').turn_started, null);
  for (const p of [{ ...packet, command: 'bad' }, { ...packet, reported: 'claimed' },
    { ...packet, message: 'x'.repeat(1025) }, { ...packet, message: 'raw diagnostic' },
    { ...packet, session: 'wrong' }, { ...packet, delivery_id: 'wrong' }]) {
    assert.throws(() => validatePacket(p, 'original'));
  }
  const empty = { ...packet, message: null, delivery_id: null };
  let calls = 0;
  deliver({ sendMessage() { calls++; } }, context(), empty);
  assert.equal(calls, 0);
});
test('configuration is explicit private pinned owner authority', t => {
  const f = config(t);
  assert.equal(configuration(f.file).af, f.value.af);
  for (const bad of [{ ...f.value, command: 'bad' }, { ...f.value, af_digest: id },
    { ...f.value, af: 'relative' }, { ...f.value, schema: 'old' }]) {
    fs.writeFileSync(f.file, JSON.stringify(bad));
    assert.throws(() => configuration(f.file));
  }
  fs.writeFileSync(f.file, JSON.stringify(f.value));
  fs.chmodSync(f.file, 0o644);
  assert.throws(() => configuration(f.file));
  fs.chmodSync(f.file, 0o600);
  const link = path.join(f.root, 'linked.json');
  fs.symlinkSync(f.file, link);
  assert.throws(() => configuration(link));
});
test('host hook records argv, waits while busy and stops after uncertain native call', async t => {
  const f = config(t);
  const commands = new Map();
  const events = new Map();
  const calls = [];
  let tick;
  let stopped = 0;
  let idle = false;
  const originalExec = child.execFile;
  const originalInterval = globalThis.setInterval;
  const originalClear = globalThis.clearInterval;
  t.after(() => {
    child.execFile = originalExec;
    globalThis.setInterval = originalInterval;
    globalThis.clearInterval = originalClear;
    syncBuiltinESMExports();
  });
  child.execFile = (binary, args, options, done) => {
    calls.push({ binary, args, options });
    done(null, JSON.stringify(packet));
    return {};
  };
  syncBuiltinESMExports();
  globalThis.setInterval = callback => { tick = callback; return { unref() {} }; };
  globalThis.clearInterval = () => { stopped++; };
  const native = [];
  extension({ registerCommand: (n, v) => commands.set(n, v), on: (n, cb) => events.set(n, cb),
    sendMessage: (...a) => { native.push(a); throw Error('lost return after native admission'); } });
  const ctx = { ...context(), isIdle: () => idle };
  await commands.get('af-wake-connect').handler(f.file, ctx);
  assert.equal(calls.length, 0, 'busy host retains durable inbox until idle');
  idle = true;
  tick();
  assert.equal(calls.length, 1);
  assert.equal(native.length, 1);
  assert.equal(calls[0].binary, f.value.af);
  assert.deepEqual(calls[0].args, ['task', 'wake', '--repo', f.value.repo, '--state', f.value.state,
    'pi-next', '--route', f.value.route, '--session', 'original']);
  assert.equal(calls[0].options.timeout, 5000);
  assert.equal(calls[0].options.maxBuffer, 16384);
  assert.ok(stopped > 0);
  await events.get('session_shutdown')();
  assert.equal(native.length, 1);
});

function delayedHook(t) {
  const f = config(t);
  const commands = new Map();
  const events = new Map();
  const pending = [];
  const timers = new Set();
  const sends = [];
  const notices = [];
  const originalExec = child.execFile;
  const originalInterval = globalThis.setInterval;
  const originalClear = globalThis.clearInterval;
  t.after(() => {
    child.execFile = originalExec;
    globalThis.setInterval = originalInterval;
    globalThis.clearInterval = originalClear;
    syncBuiltinESMExports();
  });
  child.execFile = (_binary, _args, _options, done) => { pending.push(done); return {}; };
  syncBuiltinESMExports();
  globalThis.setInterval = tick => { const timer = { tick, unref() {} }; timers.add(timer); return timer; };
  globalThis.clearInterval = timer => { assert.ok(timers.delete(timer)); };
  extension({ registerCommand: (n, v) => commands.set(n, v), on: (n, cb) => events.set(n, cb),
    sendMessage: (...args) => sends.push(args) });
  const ctx = { ...context(), ui: { notify: (...args) => notices.push(args) } };
  return { ...f, commands, events, pending, timers, sends, notices,
    ctx, connect: () => commands.get('af-wake-connect').handler(f.file, ctx) };
}
test('delayed packet queues followUp when original attachment becomes busy', async t => {
  const hook = delayedHook(t);
  await hook.connect();
  assert.equal(hook.pending.length, 1, 'poll starts while idle');
  hook.ctx.isIdle = () => false;
  hook.pending[0](null, JSON.stringify(packet));
  assert.equal(hook.sends.length, 1, 'claimed packet is queued despite intervening host activity');
  assert.deepEqual(hook.sends[0][1], { triggerTurn: true, deliverAs: 'followUp' });
  assert.equal(hook.sends[0][0].details.delivery_id, packet.delivery_id);
  assert.equal(hook.timers.size, 0, 'native attempt remains uncertain and is never repeated');
});
for (const stop of ['disconnect', 'shutdown']) {
  for (const response of ['success', 'error']) {
    test(`delayed ${response} callback has no authority after ${stop}`, async t => {
      const hook = delayedHook(t);
      await hook.connect();
      assert.equal(hook.pending.length, 1);
      const oldTick = [...hook.timers][0].tick;
      if (stop === 'shutdown') await hook.events.get('session_shutdown')();
      else await hook.commands.get('af-wake-disconnect').handler();
      const notices = hook.notices.length;
      hook.pending[0](response === 'error' ? Error('lost subprocess return') : null, JSON.stringify(packet));
      oldTick();
      assert.equal(hook.sends.length, 0, 'a possibly committed intent stays uncertain without sending');
      assert.equal(hook.notices.length, notices, 'stale errors cannot mutate attachment UI');
      assert.equal(hook.timers.size, 0);
      assert.equal(hook.pending.length, 1, 'a queued stale interval cannot poll again');
    });
  }
}
for (const response of ['success', 'empty', 'error']) {
  test(`old ${response} callback cannot send or alter replacement polling`, async t => {
    const hook = delayedHook(t);
    await hook.connect();
    const oldTick = [...hook.timers][0].tick;
    await hook.connect();
    assert.equal(hook.pending.length, 2, 'replacement has its own in-flight operation');
    const replacement = [...hook.timers][0];
    const notices = hook.notices.length;
    hook.pending[0](response === 'error' ? Error('old failure') : null,
      JSON.stringify(response === 'empty' ? { ...packet, message: null, delivery_id: null } : packet));
    oldTick();
    replacement.tick();
    assert.equal(hook.pending.length, 2, 'old callback cannot clear replacement in-flight state');
    assert.equal(hook.sends.length, 0);
    assert.equal(hook.notices.length, notices);
    assert.deepEqual([...hook.timers], [replacement], 'old callback cannot stop the replacement timer');
    hook.pending[1](null, JSON.stringify(packet));
    assert.equal(hook.sends.length, 1, 'only the current attachment can send');
    assert.equal(hook.timers.size, 0, 'current uncertain native attempt still stops polling');
    replacement.tick();
    assert.equal(hook.pending.length, 2);
  });
}
