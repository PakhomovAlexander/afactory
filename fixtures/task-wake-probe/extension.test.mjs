import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { createHash } from 'node:crypto';
import child from 'node:child_process';
import { syncBuiltinESMExports } from 'node:module';
import extension, { configuration, validatePacket, deliver } from '../../scripts/task-wake-extension.ts';

// This sandbox maps system-root ownership to nobody. Model the production root-owned
// system ancestors explicitly; all fixture descendants retain their real metadata.
const actualLstat = fs.lstatSync;
const systemPaths = new Set(['/', fs.realpathSync(os.tmpdir())]);
fs.lstatSync = (file, ...args) => {
  const info = actualLstat(file, ...args);
  if (systemPaths.has(file)) info.uid = 0;
  return info;
};
syncBuiltinESMExports();

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

test('successful deferred empty polls retain the same bounded attachment until completion', async t => {
  const hook = delayedHook(t);
  await hook.connect();
  const timer = [...hook.timers][0];
  for (let poll = 0; poll < 3; poll++) {
    hook.pending[poll](null, JSON.stringify({ ...packet, message: null, delivery_id: null }));
    assert.deepEqual([...hook.timers], [timer]);
    assert.equal(hook.sends.length, 0);
    timer.tick();
    assert.equal(hook.pending.length, poll + 2);
  }
  hook.pending[3](null, JSON.stringify(packet));
  assert.equal(hook.sends.length, 1, 'completion needs no reconnect or external drain');
  assert.equal(hook.timers.size, 0, 'native uncertainty still stops polling');
});
test('empty deferred polls cannot extend the finite attachment allowance', async t => {
  const hook = delayedHook(t);
  await hook.connect();
  const timer = [...hook.timers][0];
  for (let poll = 0; poll < 128; poll++) {
    hook.pending[poll](null, JSON.stringify({ ...packet, message: null, delivery_id: null }));
    timer.tick();
  }
  assert.equal(hook.pending.length, 128);
  assert.equal(hook.timers.size, 0);
  assert.equal(hook.sends.length, 0);
});

for (const field of ['af', 'af_digest', 'repo', 'state', 'route']) {
  test(`captured attachment refuses delayed ${field} configuration drift`, async t => {
    const hook = delayedHook(t);
    await hook.connect();
    hook.pending[0](null, JSON.stringify({ ...packet, message: null, delivery_id: null }));
    const timer = [...hook.timers][0];
    const changed = { ...hook.value };
    if (field === 'af' || field === 'af_digest') {
      const other = path.join(hook.root, 'other-af');
      fs.writeFileSync(other, '#!/bin/sh\nexit 2\n', { mode: 0o700 });
      changed.af = field === 'af' ? other : hook.value.af;
      if (field === 'af_digest') fs.writeFileSync(changed.af, '#!/bin/sh\nexit 4\n');
      changed.af_digest = 'sha256:' + createHash('sha256').update(fs.readFileSync(changed.af)).digest('hex');
      // Reproduce validate-B/execute-A, including replacement of captured A.
      if (field === 'af') fs.writeFileSync(hook.value.af, '#!/bin/sh\nexit 3\n');
    } else {
      const other = path.join(hook.root, 'other-' + field);
      if (field === 'route') fs.writeFileSync(other, '{}', { mode: 0o600 });
      else fs.mkdirSync(other, { mode: 0o700 });
      changed[field] = other;
    }
    fs.writeFileSync(hook.file, JSON.stringify(changed));
    timer.tick();
    assert.equal(hook.pending.length, 1, 'no superseded subprocess authority');
    assert.equal(hook.timers.size, 0);
    assert.equal(hook.sends.length, 0);
    timer.tick();
    assert.equal(hook.pending.length, 1);
  });
}
test('configuration drift invalidates a pending successful native callback', async t => {
  const hook = delayedHook(t);
  await hook.connect();
  fs.writeFileSync(hook.file, JSON.stringify({ ...hook.value, af_digest: id }));
  hook.pending[0](null, JSON.stringify(packet));
  assert.equal(hook.sends.length, 0);
  assert.equal(hook.timers.size, 0);
});
for (const level of ['immediate', 'intermediate']) {
  test(`executable refuses replaceable ${level} ancestry and permits safely sticky ancestry`, t => {
    const f = config(t);
    const outer = path.join(f.root, 'outer');
    const inner = path.join(outer, 'inner');
    fs.mkdirSync(inner, { recursive: true, mode: 0o700 });
    const binary = path.join(inner, 'af');
    fs.copyFileSync(f.value.af, binary);
    fs.chmodSync(binary, 0o700);
    fs.writeFileSync(f.file, JSON.stringify({ ...f.value, af: binary }));
    assert.equal(configuration(f.file).af, binary);
    const unsafe = level === 'immediate' ? inner : outer;
    fs.chmodSync(unsafe, 0o777);
    assert.throws(() => configuration(f.file));
    fs.chmodSync(unsafe, 0o1777);
    assert.equal(configuration(f.file).af, binary, 'sticky owner protects trusted child');
  });
}

test('ancestry replacement between validation and delayed launch revokes subprocess authority', async t => {
  const hook = delayedHook(t);
  await hook.connect();
  hook.pending[0](null, JSON.stringify({ ...packet, message: null, delivery_id: null }));
  const tick = [...hook.timers][0].tick;
  // The first call completed; expose the exact captured executable to other-UID replacement
  // before the next launch. Keep the config itself private in a separate directory.
  const privateConfig = path.join(hook.root, 'private-config');
  fs.mkdirSync(privateConfig, { mode: 0o700 });
  const binaryParent = path.join(hook.root, 'binary-parent');
  fs.mkdirSync(binaryParent, { mode: 0o700 });
  const other = path.join(binaryParent, 'af');
  fs.copyFileSync(hook.value.af, other);
  fs.chmodSync(other, 0o700);
  const otherFile = path.join(privateConfig, 'config.json');
  fs.writeFileSync(otherFile, JSON.stringify({ ...hook.value, af: other }), { mode: 0o600 });
  await hook.commands.get('af-wake-connect').handler(otherFile, hook.ctx);
  const timer = [...hook.timers][0];
  hook.pending[1](null, JSON.stringify({ ...packet, message: null, delivery_id: null }));
  fs.chmodSync(binaryParent, 0o777);
  timer.tick();
  tick();
  assert.equal(hook.pending.length, 2, 'no call through a newly replaceable pathname');
  assert.equal(hook.timers.size, 0);
  assert.equal(hook.sends.length, 0);
});
test('sticky directory owned by a foreign UID does not protect the executable', t => {
  const f = config(t);
  const foreignParent = path.join(f.root, 'foreign-sticky');
  fs.mkdirSync(foreignParent, { mode: 0o1777 });
  const binary = path.join(foreignParent, 'af');
  fs.copyFileSync(f.value.af, binary);
  fs.chmodSync(binary, 0o700);
  fs.writeFileSync(f.file, JSON.stringify({ ...f.value, af: binary }));
  const original = fs.lstatSync;
  t.after(() => { fs.lstatSync = original; syncBuiltinESMExports(); });
  fs.lstatSync = (file, ...args) => {
    const info = original(file, ...args);
    if (file === foreignParent) { info.uid = 424242; info.mode = 0o41777; }
    return info;
  };
  syncBuiltinESMExports();
  assert.throws(() => configuration(f.file));
});

test('trusted executable hard links retain safe pathname authority', t => {
  const f = config(t);
  const alias = path.join(f.root, 'af-alias');
  fs.linkSync(f.value.af, alias);
  assert.equal(configuration(f.file).af, f.value.af);
  fs.writeFileSync(f.file, JSON.stringify({ ...f.value, af: alias }));
  assert.equal(configuration(f.file).af, alias);
});
