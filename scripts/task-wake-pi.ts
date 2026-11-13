/** Marker-only Pi experiment. No listener, watcher, model tool or automatic retry. */
import type { ExtensionAPI, ExtensionCommandContext } from '@earendil-works/pi-coding-agent';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { createHash } from 'node:crypto';

type Packet = {
  schema: 'af/task-wake-probe@1'; run_id: string; host: string; session_id: string;
  probe_id: string; delivery_id: string; scenario: string; marker: string;
};
type Receipt = Record<string, string | null>;
type Context = Pick<ExtensionCommandContext, 'sessionManager' | 'isIdle'>;
type Host = Pick<ExtensionAPI, 'sendMessage'>;
const identities = ['run_id', 'host', 'session_id', 'probe_id', 'delivery_id'] as const;
const token = /^[A-Za-z0-9_-]{1,128}$(?![\s\S])/;
const limit = 2048;
export function canonical(value: Record<string, unknown>): string {
  return JSON.stringify(Object.fromEntries(Object.keys(value).sort().map(k => [k, value[k]])));
}
function fail(): never { throw new Error('probe refused: invalid input, binding, journal or capability'); }
function closed(value: unknown, keys: string[]): asserts value is Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value) ||
      Object.keys(value).sort().join(',') !== keys.sort().join(',')) fail();
}
function read(file: string, maximum: number): Buffer {
  const fd = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW | fs.constants.O_NONBLOCK);
  try {
    if (!fs.fstatSync(fd).isFile()) fail();
    const result = Buffer.alloc(maximum + 1);
    const length = fs.readSync(fd, result, 0, maximum + 1, 0);
    if (length > maximum) fail();
    return result.subarray(0, length);
  } finally { fs.closeSync(fd); }
}
function parse(raw: Buffer): unknown {
  if (raw.length > limit) fail();
  const value = JSON.parse(raw.toString('utf8'));
  if (!value || typeof value !== 'object' || Array.isArray(value) ||
      !raw.equals(Buffer.from(canonical(value) + '\n', 'ascii'))) fail();
  return value;
}
export function readPacket(file: string, session: string): Packet {
  const value = parse(read(file, limit));
  closed(value, ['schema', ...identities, 'scenario', 'marker']);
  if (value.schema !== 'af/task-wake-probe@1' || value.host !== 'pi' ||
      typeof value.scenario !== 'string' ||
      !['idle', 'busy', 'reconnect', 'restart', 'duplicate', 'ambiguous'].includes(value.scenario) ||
      typeof value.marker !== 'string' || !/^AF_WAKE_[a-f0-9]{16}$(?![\s\S])/.test(value.marker)) fail();
  for (const k of identities) if (typeof value[k] !== 'string' || !token.test(value[k])) fail();
  if (!token.test(session) || value.session_id !== session) fail();
  return value as Packet;
}
export function receipt(p: Packet, stage: string, reason: string): Receipt {
  return { schema: 'af/task-wake-evidence@1', ...Object.fromEntries(identities.map(k => [k, p[k]])),
    payload_sha256: 'sha256:' + createHash('sha256').update(canonical(p)).digest('hex'),
    stage, reason, reference: null };
}
function privateDirectory(directory: string): string {
  const resolved = path.resolve(directory);
  const info = fs.lstatSync(resolved);
  if (!info.isDirectory() || fs.realpathSync(resolved) !== resolved ||
      info.uid !== process.getuid?.() || (info.mode & 0o777) !== 0o700) fail();
  return resolved;
}
function privateFile(file: string): boolean {
  let info: fs.Stats;
  try { info = fs.lstatSync(file); } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return false;
    throw error;
  }
  if (!info.isFile() || info.uid !== process.getuid?.() ||
      (info.mode & 0o777) !== 0o600 || info.nlink !== 1) fail();
  return true;
}
function syncDirectory(directory: string): void {
  const fd = fs.openSync(directory, fs.constants.O_RDONLY | fs.constants.O_DIRECTORY | fs.constants.O_NOFOLLOW);
  try { fs.fsyncSync(fd); } finally { fs.closeSync(fd); }
}
function syncFile(file: string): void {
  const fd = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW);
  try { fs.fsyncSync(fd); } finally { fs.closeSync(fd); }
}
function syncDirectoryAncestry(directory: string): void {
  // The operator may just have created any prefix of the selected canonical path.
  // Persist the inbox entry first, then every directory's entry in its parent.
  // No creation history or presumed already-durable ancestor can replace these syncs.
  for (;;) {
    syncDirectory(directory);
    const parent = path.dirname(directory);
    if (parent === directory) return;
    directory = parent;
  }
}
export function runProbe(host: Host, ctx: Context, file: string, session: string,
                         journal: string, live = false): Receipt[] {
  const p = readPacket(file, session);
  if (ctx.sessionManager.getSessionId() !== session) fail();
  if (!live) return [receipt(p, 'dry_run', 'offline')];
  if (typeof host.sendMessage !== 'function' || typeof ctx.isIdle !== 'function') {
    return [receipt(p, 'unavailable', 'host_unavailable')];
  }
  const directory = privateDirectory(journal);
  const lock = path.join(directory, '.lock');
  // A crashed lock is deliberately not broken automatically; an operator must establish
  // that the old owner is gone. Never wait indefinitely or race a second native caller.
  fs.mkdirSync(lock, { mode: 0o700 });
  try {
    const state = path.join(directory, 'events.jsonl');
    const claim = path.join(directory, 'admission.claim');
    const inbox = path.join(directory, 'inbox.json');
    const retained = privateFile(inbox);
    // Retain the exact bounded packet, not just its digest. An orphan packet can be a
    // pre-receipt crash; a receipt without its packet is corrupt and cannot be replayed.
    if (retained && canonical(readPacket(inbox, session)) !== canonical(p)) fail();
    let rows: Receipt[] = [];
    if (privateFile(state)) {
      if (!retained) fail();
      const raw = read(state, 128 * limit);
      const lines = raw.toString('utf8').split('\n');
      if (lines.pop() !== '' || lines.length < 1 || lines.length > 4) fail();
      rows = lines.map(line => parse(Buffer.from(line + '\n')) as Receipt);
      const stages = [['inbox_accepted', 'persisted'], ['admission_intent', 'before_native_call']];
      if (rows.length >= 3) stages.push(rows[2].stage === 'native_returned'
        ? ['native_returned', 'no_native_receipt'] : ['admission_unknown', 'lost_ack']);
      if (rows.length === 4) {
        if (rows[2].stage !== 'native_returned') fail();
        stages.push(['admission_unknown', 'no_native_receipt']);
      }
      for (let i = 0; i < rows.length; i++) {
        if (canonical(rows[i]) !== canonical(receipt(p, stages[i][0], stages[i][1]))) fail();
      }
    }
    // A separate immutable claim detects a missing/truncated journal after native admission.
    // Destructive loss of the entire directory is never recoverable proof of absence.
    const claimed = privateFile(claim);
    if (claimed) {
      const observed = parse(read(claim, limit));
      if (canonical(observed as Receipt) !== canonical(receipt(p, 'admission_intent', 'before_native_call')) ||
          rows.length < 2) fail();
    }
    if (rows.length) {
      // A complete retained write can precede a failed fsync or process crash. Validation
      // proves its meaning, not its persistence. Re-establish all retained file contents
      // and directory entries before exposing receipts or continuing pre-admission work.
      syncFile(inbox);
      syncFile(state);
      if (claimed) syncFile(claim);
      syncDirectoryAncestry(directory);
    }
    // Durable admission intent alone blocks resubmission, including a crash between API
    // admission and recording its return. Returning retained observations is not a new receipt.
    if (rows.length >= 2) return rows;
    const idle = ctx.isIdle();
    if ((p.scenario === 'idle' && !idle) || (p.scenario === 'busy' && idle)) fail();
    const append = (stage: string, reason: string) => {
      const row = receipt(p, stage, reason);
      const fd = fs.openSync(state, fs.constants.O_WRONLY | fs.constants.O_APPEND |
        fs.constants.O_CREAT | fs.constants.O_NOFOLLOW, 0o600);
      try {
        const bytes = Buffer.from(canonical(row) + '\n');
        if (fs.writeSync(fd, bytes) !== bytes.length) fail();
        fs.fsyncSync(fd);
      } finally { fs.closeSync(fd); }
      syncDirectory(directory);
      rows.push(row);
    };
    if (!rows.length) {
      const fd = fs.openSync(inbox, retained ? fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW :
        fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL | fs.constants.O_NOFOLLOW, 0o600);
      try {
        if (!retained) {
          const bytes = Buffer.from(canonical(p) + '\n', 'ascii');
          if (fs.writeSync(fd, bytes) !== bytes.length) fail();
        }
        // Re-sync a complete orphan packet as well: a crash may have preceded its fsync.
        fs.fsyncSync(fd);
      } finally { fs.closeSync(fd); }
      syncDirectoryAncestry(directory);
      append('inbox_accepted', 'persisted');
    }
    append('admission_intent', 'before_native_call');
    const claimFd = fs.openSync(claim, fs.constants.O_WRONLY | fs.constants.O_CREAT |
      fs.constants.O_EXCL | fs.constants.O_NOFOLLOW, 0o600);
    try {
      const bytes = Buffer.from(canonical(receipt(p, 'admission_intent', 'before_native_call')) + '\n');
      if (fs.writeSync(claimFd, bytes) !== bytes.length) fail();
      fs.fsyncSync(claimFd);
    } finally { fs.closeSync(claimFd); }
    syncDirectory(directory);
    try {
      host.sendMessage({ customType: 'af-wake-probe',
        content: `Reply with this marker only; use no tools: ${p.marker}`, display: true,
        details: { delivery_id: p.delivery_id } }, { triggerTurn: true, deliverAs: 'followUp' });
    } catch {
      append('admission_unknown', 'lost_ack');
      return rows;
    }
    if (p.scenario === 'ambiguous') {
      // Deliberately discard the return observation after the one native call.
      append('admission_unknown', 'lost_ack');
    } else {
      append('native_returned', 'no_native_receipt');
      append('admission_unknown', 'no_native_receipt');
    }
    return rows;
  } finally { fs.rmdirSync(lock); }
}

export default function (pi: ExtensionAPI): void {
  pi.registerCommand('af-wake-identity', {
    description: 'Show the current opaque session ID locally; sends no model message',
    handler: async (_args, ctx) => { ctx.ui.notify(ctx.sessionManager.getSessionId(), 'info'); },
  });
  pi.registerCommand('af-wake-probe', {
    description: 'dry|live EXACT_SESSION PACKET_FILE PRIVATE_JOURNAL_DIR (paths without spaces)',
    handler: async (args, ctx) => {
      try {
        const parts = args.trim().split(/\s+/);
        if (parts.length !== 4 || !['dry', 'live'].includes(parts[0])) fail();
        const [mode, session, file, journal] = parts;
        const rows = runProbe(pi, ctx, file, session, journal, mode === 'live');
        ctx.ui.notify(rows.map(canonical).join('\n'), 'info');
      } catch {
        // Never echo raw host diagnostics, packets, credentials or transcripts.
        ctx.ui.notify('Probe refused; inspect the selected binding and bounded journal. No automatic resend.', 'error');
      }
    },
  });
}
