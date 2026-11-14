import test from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import mutableFs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { syncBuiltinESMExports } from 'node:module';
import extension, { canonical, runProbe, receipt } from '../../scripts/task-wake-pi.ts';
const fixture = JSON.parse(fs.readFileSync(new URL('pi.json', import.meta.url)));
function setup(t, scenario = 'idle', temporary = os.tmpdir(), nested = '') {
  const base = fs.realpathSync(fs.mkdtempSync(path.join(temporary, 'af-wake-fixture-')));
  t.after(() => fs.rmSync(base, { recursive: true, force: true }));
  const root = path.join(base, nested);
  fs.mkdirSync(root, { recursive: true, mode: 0o700 });
  fs.chmodSync(root, 0o700);
  const file = path.join(root, 'packet.json');
  const p = { ...fixture, scenario };
  fs.writeFileSync(file, canonical(p) + '\n');
  const calls = [];
  const ctx = { sessionManager: { getSessionId: () => p.session_id }, isIdle: () => scenario !== 'busy' };
  const api = { sendMessage: (...args) => calls.push(args) };
  const run = (live = true) => runProbe(api, ctx, file, p.session_id, root, live);
  return { root, file, p, calls, ctx, api, run };
}
function retain(f) {
  fs.writeFileSync(path.join(f.root, 'inbox.json'), canonical(f.p) + '\n', { mode: 0o600 });
}
function withRecordingFs(hooks, action) {
  const original = Object.fromEntries(Object.keys(hooks).map(key => [key, mutableFs[key]]));
  try {
    for (const key of Object.keys(hooks)) mutableFs[key] = (...args) => hooks[key](original[key], ...args);
    syncBuiltinESMExports();
    return action();
  } finally {
    Object.assign(mutableFs, original);
    syncBuiltinESMExports();
  }
}
function directoryAncestry(directory) {
  const root = path.parse(directory).root;
  const parts = path.relative(root, directory).split(path.sep);
  return [...parts.map((_, i) => path.join(root, ...parts.slice(0, i + 1))).reverse(), root];
}
function recordingJournal(f) {
  const files = ['inbox.json', 'events.jsonl', 'admission.claim'].map(name => path.join(f.root, name));
  const contents = new Map();
  let entries = new Set();
  const syncs = [];
  return {
    syncs,
    files,
    record(action, { afterWrite = () => {}, beforeSync = () => {} } = {}) {
      const descriptors = new Map();
      return withRecordingFs({
        openSync: (original, file, ...args) => {
          const fd = original(file, ...args);
          descriptors.set(fd, file);
          return fd;
        },
        writeSync: (original, fd, bytes, ...args) => {
          const length = original(fd, bytes, ...args);
          assert.equal(length, bytes.length);
          afterWrite(descriptors.get(fd), bytes);
          return length;
        },
        fsyncSync: (original, fd) => {
          const file = descriptors.get(fd);
          beforeSync(file);
          original(fd);
          syncs.push(file);
          if (files.includes(file)) contents.set(file, fs.readFileSync(file));
          if (file === f.root) entries = new Set(files.filter(file => fs.existsSync(file)));
        },
      }, action);
    },
    powerLoss() {
      // Model file contents and leaf directory entries independently. Unsynced complete
      // writes disappear, including appends to an already durable file. Ancestor ordering
      // is asserted separately; this is a recording model, not a real host crash trial.
      for (const file of files) {
        if (entries.has(file) && contents.has(file)) {
          fs.writeFileSync(file, contents.get(file), { mode: 0o600 });
        } else fs.rmSync(file, { force: true });
      }
    },
  };
}
const retainedBoundaries = [
  ['inbox.json', null, 0],
  ['events.jsonl', 'inbox_accepted', 0],
  ['events.jsonl', 'admission_intent', 0],
  ['admission.claim', 'admission_intent', 0],
  ['events.jsonl', 'native_returned', 1],
  ['events.jsonl', 'admission_unknown', 1],
];
function interruptCompleteWrite(f, disk, boundary, mode) {
  const [name, stage, calls] = boundary;
  let written = false;
  disk.record(() => assert.throws(() => f.run(), /injected persistence failure/), {
    afterWrite: (file, bytes) => {
      if (path.basename(file) === name && (!stage || JSON.parse(bytes).stage === stage)) {
        written = true;
        if (mode === 'pre-fsync crash') throw new Error('injected persistence failure');
      }
    },
    beforeSync: file => {
      if (written && file === (mode === 'directory fsync failure' ? f.root : path.join(f.root, name))) {
        throw new Error('injected persistence failure');
      }
    },
  });
  assert.ok(written);
  assert.equal(f.calls.length, calls);
  assert.ok(!fs.existsSync(path.join(f.root, '.lock')));
  return disk.files.filter(file => fs.existsSync(file));
}
function assertAcceptanceDurability(f) {
  const descriptors = new Map();
  const synced = [];
  const expected = [path.join(f.root, 'inbox.json'), ...directoryAncestry(f.root)];
  withRecordingFs({
    openSync: (original, file, ...args) => {
      const fd = original(file, ...args);
      descriptors.set(fd, file);
      return fd;
    },
    fsyncSync: (original, fd) => {
      original(fd);
      synced.push(descriptors.get(fd));
    },
    writeSync: (original, fd, bytes, ...args) => {
      if (bytes.toString().includes('"stage":"inbox_accepted"')) {
        assert.deepEqual(synced, expected);
        synced.push('inbox_receipt_written');
      }
      return original(fd, bytes, ...args);
    },
  }, () => assert.equal(f.run()[0].stage, 'inbox_accepted'));
  assert.deepEqual(synced.slice(0, expected.length + 3),
    [...expected, 'inbox_receipt_written', path.join(f.root, 'events.jsonl'), f.root]);
  assert.equal(f.calls.length, 1);
}
test('fixture canonicalizes a temporary directory with a symlinked ancestor', t => {
  const parent = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'af-wake-parent-')));
  t.after(() => fs.rmSync(parent, { recursive: true, force: true }));
  fs.mkdirSync(path.join(parent, 'actual'));
  fs.symlinkSync(path.join(parent, 'actual'), path.join(parent, 'alias'));
  const f = setup(t, 'idle', path.join(parent, 'alias'));
  assert.equal(f.root, fs.realpathSync(f.root));
  assert.ok(f.root.startsWith(path.join(parent, 'actual') + path.sep));
  // Raw aliases still fail runtime admission, even when only an ancestor is a symlink.
  const alias = path.join(parent, 'alias', path.basename(f.root));
  assert.throws(() => runProbe(f.api, f.ctx, f.file, f.p.session_id, alias, true));
  assert.equal(f.calls.length, 0);
  assert.equal(f.run()[0].stage, 'inbox_accepted');
  assert.equal(f.calls.length, 1);
});
for (const scenario of ['idle', 'busy', 'reconnect', 'restart', 'duplicate', 'ambiguous']) {
  test(`Pi ${scenario}: exact binding, followUp and durable no-resend`, t => {
    const f = setup(t, scenario);
    const first = f.run();
    assert.equal(f.calls.length, 1);
    assert.deepEqual(f.calls[0][1], { triggerTurn: true, deliverAs: 'followUp' });
    assert.equal(f.calls[0][0].content, `Reply with this marker only; use no tools: ${f.p.marker}`);
    assert.equal(first[0].stage, 'inbox_accepted');
    assert.equal(first[1].stage, 'admission_intent');
    assert.equal(first.at(-1).stage, 'admission_unknown');
    // A newly constructed host/context simulates a restarted bridge: disk state owns dedup.
    const reopened = runProbe({ sendMessage: () => assert.fail('resend') }, f.ctx,
      f.file, f.p.session_id, f.root, true);
    assert.deepEqual(reopened, first);
    assert.equal(f.calls.length, 1);
    assert.ok(!first.some(r => ['native_admitted', 'turn_started', 'report_observed'].includes(r.stage)));
  });
}
test('lost acknowledgement and crash after persisted intent block retries', t => {
  const f = setup(t);
  f.api.sendMessage = () => { f.calls.push('accepted'); throw new Error('lost private response'); };
  assert.equal(f.run().at(-1).reason, 'lost_ack');
  f.run();
  assert.equal(f.calls.length, 1);
  const rows = [receipt(f.p, 'inbox_accepted', 'persisted'), receipt(f.p, 'admission_intent', 'before_native_call')];
  fs.writeFileSync(path.join(f.root, 'events.jsonl'), rows.map(canonical).join('\n') + '\n', { mode: 0o600 });
  assert.deepEqual(f.run(), rows);
  assert.equal(f.calls.length, 1);
});
test('crash before intent may continue; no API without persisted intent', t => {
  const f = setup(t);
  retain(f);
  const row = receipt(f.p, 'inbox_accepted', 'persisted');
  fs.writeFileSync(path.join(f.root, 'events.jsonl'), canonical(row) + '\n', { mode: 0o600 });
  f.api.sendMessage = () => {
    const journal = fs.readFileSync(path.join(f.root, 'events.jsonl'), 'utf8');
    assert.equal(JSON.parse(journal.trim().split('\n').at(-1)).stage, 'admission_intent');
    f.calls.push('native');
  };
  f.run();
  assert.equal(f.calls.length, 1);
});
test('inbox acceptance retains the canonical payload independently of the supplied file', t => {
  const f = setup(t);
  const inbox = path.join(f.root, 'inbox.json');
  f.api.sendMessage = () => {
    assert.equal(fs.readFileSync(inbox, 'utf8'), canonical(f.p) + '\n');
    assert.equal(fs.statSync(inbox).mode & 0o777, 0o600);
    f.calls.push('native');
  };
  const rows = f.run();
  fs.unlinkSync(f.file);
  // The retained packet is usable as an explicitly selected input after external loss.
  assert.deepEqual(runProbe(f.api, f.ctx, inbox, f.p.session_id, f.root, true), rows);
  assert.equal(f.calls.length, 1);
});
test('packet file and directory are fsynced before the inbox receipt is written', t => {
  assertAcceptanceDurability(setup(t));
});
test('new nested journals and complete orphan packets sync every ancestor before acceptance', t => {
  for (const orphan of [false, true]) {
    const f = setup(t, 'idle', os.tmpdir(), path.join('new-parent', 'new-child', 'journal'));
    if (orphan) retain(f);
    assertAcceptanceDurability(f);
  }
});
test('ancestor sync failures prevent receipts and native calls; explicit retry restores full durability', t => {
  const nested = path.join('new-parent', 'new-child', 'journal');
  const depth = directoryAncestry(setup(t, 'idle', os.tmpdir(), nested).root).length;
  for (let ancestor = 1; ancestor < depth; ancestor++) {
    for (const after of [false, true]) {
      const f = setup(t, 'idle', os.tmpdir(), nested);
      const failedDirectory = directoryAncestry(f.root)[ancestor];
      const descriptors = new Map();
      let injected = false;
      withRecordingFs({
        openSync: (original, file, ...args) => {
          const fd = original(file, ...args);
          descriptors.set(fd, file);
          return fd;
        },
        fsyncSync: (original, fd) => {
          if (descriptors.get(fd) === failedDirectory) {
            injected = true;
            if (after) original(fd);
            throw new Error('injected ancestor sync failure');
          }
          original(fd);
        },
      }, () => assert.throws(() => f.run(), /injected ancestor sync failure/));
      assert.ok(injected);
      assert.equal(f.calls.length, 0);
      assert.ok(!fs.existsSync(path.join(f.root, 'events.jsonl')));
      assert.ok(!fs.existsSync(path.join(f.root, 'admission.claim')));
      assert.ok(!fs.existsSync(path.join(f.root, '.lock')));
      assert.equal(fs.readFileSync(path.join(f.root, 'inbox.json'), 'utf8'), canonical(f.p) + '\n');
      // Retry must validate/re-sync the complete orphan, including ancestors already synced.
      assertAcceptanceDurability(f);
      f.run();
      assert.equal(f.calls.length, 1);
    }
  }
});
test('crashes around packet and directory fsync recover only after durable acceptance', t => {
  for (const boundary of ['packet', 'directory']) {
    for (const after of [false, true]) {
      const f = setup(t);
      let injected = false;
      withRecordingFs({ fsyncSync: (original, fd) => {
        const directory = fs.fstatSync(fd).isDirectory();
        if (!injected && directory === (boundary === 'directory')) {
          injected = true;
          if (after) original(fd);
          throw new Error('injected crash');
        }
        original(fd);
      } }, () => assert.throws(() => f.run(), /injected crash/));
      assert.ok(injected);
      assert.equal(f.calls.length, 0);
      assert.ok(!fs.existsSync(path.join(f.root, 'events.jsonl')));
      // A complete orphan packet is validated and fsynced again on explicit replay.
      assert.equal(f.run()[0].stage, 'inbox_accepted');
      assert.equal(f.calls.length, 1);
    }
  }
});
test('crash after inbox acceptance can continue from the retained payload', t => {
  const f = setup(t);
  withRecordingFs({ writeSync: (original, fd, bytes, ...args) => {
    if (bytes.toString().includes('"stage":"admission_intent"')) throw new Error('injected crash');
    return original(fd, bytes, ...args);
  } }, () => assert.throws(() => f.run(), /injected crash/));
  assert.equal(f.calls.length, 0);
  const state = path.join(f.root, 'events.jsonl');
  assert.equal(JSON.parse(fs.readFileSync(state, 'utf8')).stage, 'inbox_accepted');
  fs.unlinkSync(f.file);
  const rows = runProbe(f.api, f.ctx, path.join(f.root, 'inbox.json'), f.p.session_id, f.root, true);
  assert.equal(rows[1].stage, 'admission_intent');
  assert.equal(f.calls.length, 1);
});
test('crash after native call preserves intent and never resends on restart', t => {
  const f = setup(t);
  withRecordingFs({ writeSync: (original, fd, bytes, ...args) => {
    if (bytes.toString().includes('"stage":"native_returned"')) throw new Error('injected crash');
    return original(fd, bytes, ...args);
  } }, () => assert.throws(() => f.run(), /injected crash/));
  assert.equal(f.calls.length, 1);
  const rows = f.run();
  assert.deepEqual(rows.map(row => row.stage), ['inbox_accepted', 'admission_intent']);
  assert.equal(f.calls.length, 1);
});
for (const mode of ['pre-fsync crash', 'file fsync failure', 'directory fsync failure']) {
  test(`complete retained writes recover durability after ${mode} without resending`, t => {
    for (const boundary of retainedBoundaries) {
      const f = setup(t, 'idle', os.tmpdir(), path.join('new-parent', 'journal'));
      const disk = recordingJournal(f);
      const retained = interruptCompleteWrite(f, disk, boundary, mode);
      const before = new Map(retained.map(file => [file, fs.readFileSync(file)]));
      const priorRows = before.get(path.join(f.root, 'events.jsonl'))?.toString().trim().split('\n').map(JSON.parse) ?? [];
      const expectedSyncs = [...retained, ...directoryAncestry(f.root)];
      disk.syncs.length = 0;
      const rows = disk.record(() => f.run(), {
        afterWrite: () => assert.deepEqual(disk.syncs.slice(0, expectedSyncs.length), expectedSyncs),
      });
      assert.deepEqual(disk.syncs.slice(0, expectedSyncs.length), expectedSyncs);
      if (priorRows.length >= 2) {
        assert.deepEqual(rows, priorRows);
        assert.deepEqual(disk.syncs, expectedSyncs);
        assert.equal(f.calls.length, boundary[2]);
        for (const [file, bytes] of before) assert.deepEqual(fs.readFileSync(file), bytes);
      } else assert.equal(f.calls.length, 1); // Only a proven pre-intent interruption may continue.
      const recovered = new Map(disk.files.filter(file => fs.existsSync(file)).map(file => [file, fs.readFileSync(file)]));
      disk.powerLoss();
      for (const [file, bytes] of recovered) assert.deepEqual(fs.readFileSync(file), bytes);
      const calls = f.calls.length;
      assert.deepEqual(disk.record(() => f.run()), rows);
      assert.equal(f.calls.length, calls);
      assert.ok(!rows.some(row => ['native_admitted', 'turn_started', 'report_observed'].includes(row.stage)));
    }
  });
}
test('every retained file and ancestor recovery sync fails closed without clearing the no-resend fence', t => {
  for (const boundary of retainedBoundaries) {
    // Each case gets a fresh complete pre-fsync write. Fail every recovery sync in turn.
    const count = directoryAncestry(setup(t).root).length +
      (boundary[0] === 'inbox.json' ? 1 : boundary[0] === 'admission.claim' || boundary[2] ? 3 : 2);
    for (let target = 0; target < count; target++) {
      const f = setup(t);
      const disk = recordingJournal(f);
      const retained = interruptCompleteWrite(f, disk, boundary, 'pre-fsync crash');
      const expected = [...retained, ...directoryAncestry(f.root)];
      assert.equal(expected.length, count);
      const before = new Map(retained.map(file => [file, fs.readFileSync(file)]));
      disk.syncs.length = 0;
      disk.record(() => assert.throws(() => f.run(), /injected recovery failure/), {
        beforeSync: file => {
          if (file === expected[target]) throw new Error('injected recovery failure');
        },
        afterWrite: () => assert.fail('recovery must finish before any new write'),
      });
      assert.deepEqual(disk.syncs, expected.slice(0, target));
      assert.equal(f.calls.length, boundary[2]);
      for (const [file, bytes] of before) assert.deepEqual(fs.readFileSync(file), bytes);
      // Even a power loss after failed recovery cannot permit a second native call.
      // A pre-native incomplete intent may instead fall back to inbox-only state.
      if (boundary[2]) disk.powerLoss();
      const rows = disk.record(() => f.run());
      const calls = f.calls.length;
      disk.powerLoss();
      assert.deepEqual(disk.record(() => f.run()), rows);
      assert.equal(f.calls.length, calls);
      if (before.has(path.join(f.root, 'admission.claim')) || boundary[1] === 'admission_intent') {
        assert.equal(calls, boundary[2]);
      }
    }
  }
});
test('missing, corrupt or altered retained packets refuse both acceptance and replay', t => {
  const corruptions = [
    inbox => fs.unlinkSync(inbox),
    inbox => fs.writeFileSync(inbox, '{'),
    inbox => fs.writeFileSync(inbox, 'x'.repeat(2049)),
    inbox => fs.writeFileSync(inbox, canonical({ ...fixture, extra: 'unknown' }) + '\n'),
    inbox => fs.writeFileSync(inbox, canonical({ ...fixture, marker: 'AF_WAKE_1111111111111111' }) + '\n'),
    ...['run_id', 'host', 'session_id', 'probe_id', 'delivery_id'].map(key =>
      inbox => fs.writeFileSync(inbox, canonical({ ...fixture, [key]: 'other' }) + '\n')),
    inbox => fs.chmodSync(inbox, 0o644),
    inbox => { fs.unlinkSync(inbox); fs.symlinkSync('packet.json', inbox); },
    inbox => { fs.unlinkSync(inbox); fs.symlinkSync('absent.json', inbox); },
    inbox => { fs.linkSync(inbox, inbox + '.link'); },
    inbox => { fs.unlinkSync(inbox); fs.mkdirSync(inbox, { mode: 0o700 }); },
  ];
  for (const replay of [false, true]) {
    for (const corrupt of corruptions) {
      const f = setup(t);
      if (replay) f.run();
      else {
        retain(f);
        const row = receipt(f.p, 'inbox_accepted', 'persisted');
        fs.writeFileSync(path.join(f.root, 'events.jsonl'), canonical(row) + '\n', { mode: 0o600 });
      }
      corrupt(path.join(f.root, 'inbox.json'));
      assert.throws(() => f.run());
      assert.equal(f.calls.length, replay ? 1 : 0);
    }
  }
});
test('incomplete orphan packets fail closed and changed retained payloads fail receipt digest joins', t => {
  const f = setup(t);
  const inbox = path.join(f.root, 'inbox.json');
  fs.writeFileSync(inbox, '{', { mode: 0o600 });
  assert.throws(() => f.run());
  assert.equal(f.calls.length, 0);
  fs.writeFileSync(inbox, canonical(f.p) + '\n');
  f.run();
  const changed = canonical({ ...f.p, marker: 'AF_WAKE_1111111111111111' }) + '\n';
  fs.writeFileSync(inbox, changed);
  fs.writeFileSync(f.file, changed);
  assert.throws(() => f.run());
  assert.equal(f.calls.length, 1);
});
test('default is dry; unavailable API; busy mismatch; wrong session', t => {
  const f = setup(t);
  assert.equal(runProbe(f.api, f.ctx, f.file, f.p.session_id, f.root)[0].stage, 'dry_run');
  assert.deepEqual(fs.readdirSync(f.root), ['packet.json']);
  assert.equal(runProbe({}, f.ctx, f.file, f.p.session_id, f.root, true)[0].stage, 'unavailable');
  f.ctx.isIdle = () => false;
  assert.throws(() => f.run());
  f.ctx.sessionManager.getSessionId = () => 'other';
  assert.throws(() => f.run());
  assert.equal(f.calls.length, 0);
});
test('unknown fields, oversized input, duplicate keys, unsafe journal, altered payload', t => {
  const f = setup(t);
  const original = fs.readFileSync(f.file);
  for (const raw of ['x'.repeat(2049), '{', canonical({ ...f.p, callback: 'bad' }) + '\n',
    canonical({ ...f.p, marker: f.p.marker + '\n' }) + '\n',
    canonical({ ...f.p, scenario: ['idle'] }) + '\n',
    '{"host":"pi","host":"claude"}\n']) {
    fs.writeFileSync(f.file, raw);
    assert.throws(() => f.run());
  }
  fs.writeFileSync(f.file, original);
  fs.chmodSync(f.root, 0o755);
  assert.throws(() => f.run());
  fs.chmodSync(f.root, 0o700);
  fs.mkdirSync(path.join(f.root, '.lock'));
  assert.throws(() => f.run());
  fs.rmdirSync(path.join(f.root, '.lock'));
  assert.equal(f.calls.length, 0);
  f.run();
  fs.writeFileSync(f.file, canonical({ ...f.p, marker: 'AF_WAKE_1111111111111111' }) + '\n');
  assert.throws(() => f.run());
  fs.writeFileSync(f.file, original);
  fs.appendFileSync(path.join(f.root, 'events.jsonl'), 'truncated');
  assert.throws(() => f.run());
  assert.equal(f.calls.length, 1);
  fs.unlinkSync(path.join(f.root, 'events.jsonl'));
  assert.throws(() => f.run());
  assert.equal(f.calls.length, 1);
});
test('registered extension command defaults to no native work', async t => {
  const f = setup(t);
  const commands = new Map();
  extension({ ...f.api, registerCommand: (name, command) => commands.set(name, command) });
  const notices = [];
  f.ctx.ui = { notify: (...args) => notices.push(args) };
  await commands.get('af-wake-identity').handler('', f.ctx);
  assert.equal(notices[0][0], f.p.session_id);
  await commands.get('af-wake-probe').handler(`dry ${f.p.session_id} ${f.file} ${f.root}`, f.ctx);
  await commands.get('af-wake-probe').handler(`${f.p.session_id} ${f.file} ${f.root}`, f.ctx);
  assert.equal(f.calls.length, 0);
  assert.equal(notices.at(-1)[1], 'error');
});
