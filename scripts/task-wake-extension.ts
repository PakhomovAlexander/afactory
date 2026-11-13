/** Production Pi 1.4.0 pull bridge. Rust's FULL-synchronous Store owns all durable state.
 * No JSON file journal, steering, SDK session, login, or native retry is used here. */
import type { ExtensionAPI, ExtensionCommandContext } from '@earendil-works/pi-coding-agent';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { createHash } from 'node:crypto';
import { execFile } from 'node:child_process';

type Configuration = { schema: 'af/task-wake-pi-config@1'; af: string; af_digest: string;
  repo: string; state: string; route: string };
type Context = Pick<ExtensionCommandContext, 'sessionManager' | 'isIdle'> & { mode: string };
type Packet = { schema: 'af/task-wake-pi-next@1'; session: string; message: string | null;
  delivery_id: string | null; admission: string; turn_started: null; reported: null };
const digest = /^sha256:[a-f0-9]{64}$(?![\s\S])/;
const name = /^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$(?![\s\S])/;
function fail(): never { throw new Error('AF wake refused; inspect durable AF receipts. No automatic native resend.'); }
function closed(value: unknown, keys: string[]): asserts value is Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value) ||
      Object.keys(value).sort().join(',') !== keys.sort().join(',')) fail();
}
function canonical(file: string): fs.Stats {
  if (!path.isAbsolute(file) || fs.realpathSync(file) !== file) fail();
  const info = fs.lstatSync(file);
  if (info.isSymbolicLink()) fail();
  return info;
}
export function configuration(file: string): Configuration {
  const parent = canonical(path.dirname(file));
  const info = canonical(file);
  if (!parent.isDirectory() || parent.uid !== process.getuid?.() || (parent.mode & 0o077) !== 0 ||
      !info.isFile() || info.uid !== process.getuid?.() || (info.mode & 0o077) !== 0 ||
      info.nlink !== 1 || info.size > 16384) fail();
  const fd = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW | fs.constants.O_NONBLOCK);
  let value: unknown;
  try {
    const bytes = Buffer.alloc(16385);
    const length = fs.readSync(fd, bytes, 0, bytes.length, 0);
    if (length > 16384) fail();
    value = JSON.parse(bytes.subarray(0, length).toString('utf8'));
  } finally { fs.closeSync(fd); }
  closed(value, ['schema', 'af', 'af_digest', 'repo', 'state', 'route']);
  if (value.schema !== 'af/task-wake-pi-config@1' || typeof value.af_digest !== 'string' ||
      !digest.test(value.af_digest)) fail();
  for (const key of ['af', 'repo', 'state', 'route']) {
    if (typeof value[key] !== 'string') fail();
    canonical(value[key] as string);
  }
  const binary = canonical(value.af as string);
  if (!binary.isFile() || (binary.uid !== 0 && binary.uid !== process.getuid?.()) ||
      (binary.mode & 0o022) !== 0 || binary.size > 268435456 ||
      'sha256:' + createHash('sha256').update(fs.readFileSync(value.af as string)).digest('hex') !== value.af_digest) fail();
  return value as Configuration;
}
export function validatePacket(value: unknown, session: string): Packet {
  closed(value, ['schema', 'session', 'message', 'delivery_id', 'admission', 'turn_started', 'reported']);
  if (value.schema !== 'af/task-wake-pi-next@1' || value.session !== session || !name.test(session) ||
      value.admission !== 'unknown_if_packet_returned' || value.turn_started !== null || value.reported !== null) fail();
  if (value.message === null) { if (value.delivery_id !== null) fail(); }
  else if (typeof value.delivery_id !== 'string' || !digest.test(value.delivery_id) ||
           typeof value.message !== 'string' || value.message.length > 1024 ||
           !value.message.startsWith('AF Task wake metadata. Task: ') || /[\r\n\x00]/.test(value.message)) fail();
  if (value.message !== null) {
    const match = /^AF Task wake metadata[.] Task: ([A-Za-z0-9][A-Za-z0-9_-]{0,127}); Store: (sha256:[a-f0-9]{64}); receipt: /.exec(value.message as string);
    if (!match || value.message !== `AF Task wake metadata. Task: ${match[1]}; Store: ${match[2]}; receipt: ${value.delivery_id}. Inspect current Task evidence and report it under this session's existing authority. This observation may be historical, superseded or source-collected. Do not infer current completion from this wake. This wake authorizes no Task execution, resume, login, budget increase or publication.`) fail();
  }
  return value as Packet;
}
type Host = Pick<ExtensionAPI, 'sendMessage'>;
export function deliver(pi: Host, ctx: Context, packet: Packet): void {
  validatePacket(packet, ctx.sessionManager.getSessionId());
  if (ctx.mode !== 'interactive' || !ctx.isIdle()) fail();
  if (packet.message === null) return;
  // take_wake_native committed its sole native intent before this packet was emitted.
  // Returning void supplies no turn-start/report proof. Any exception is equally uncertain.
  pi.sendMessage({ customType: 'af-task-wake', content: packet.message, display: true,
    details: { delivery_id: packet.delivery_id } }, { triggerTurn: true, deliverAs: 'followUp' });
}
export default function (pi: ExtensionAPI): void {
  let timer: ReturnType<typeof setInterval> | undefined;
  let polling = false;
  const stop = () => { if (timer) clearInterval(timer); timer = undefined; };
  pi.on('session_shutdown', async () => { stop(); });
  pi.registerCommand('af-wake-connect', {
    description: 'Explicitly attach this interactive session to an owner-private AF wake config for at most 5 minutes; reconnect explicitly',
    handler: async (args, native) => {
      stop();
      const ctx = native as unknown as Context;
      try {
        if (ctx.mode !== 'interactive') fail();
        const file = args.trim();
        const config = configuration(file);
        const session = ctx.sessionManager.getSessionId();
        if (!name.test(session)) fail();
        const started = Date.now();
        let polls = 0;
        const poll = () => {
          if (++polls > 128 || Date.now() - started >= 300000 ||
              ctx.sessionManager.getSessionId() !== session) { stop(); return; }
          if (polling || !ctx.isIdle()) return;
          polling = true;
          try { configuration(file); } catch { polling = false; stop(); return; }
          // Exact argv; no shell, no ambient session ID or callback. The CLI authenticates
          // its Linux parent and protected grant, then commits durable uncertainty.
          execFile(config.af, ['task', 'wake', '--repo', config.repo, '--state', config.state,
            'pi-next', '--route', config.route, '--session', session],
          { timeout: 5000, maxBuffer: 16384, cwd: '/', encoding: 'utf8' }, (error, stdout) => {
            polling = false;
            try {
              if (error) fail();
              const packet = validatePacket(JSON.parse(stdout), session);
              if (packet.message !== null) { deliver(pi, ctx, packet); stop(); }
            } catch { stop(); native.ui.notify('AF wake blocked or uncertain; inspect `af task wake inspect`. No native resend.', 'error'); }
          });
        };
        timer = setInterval(poll, 2000);
        timer.unref?.();
        poll();
        native.ui.notify('AF wake hook attached for up to five minutes with bounded catch-up. Native receipts remain independently evidenced.', 'info');
      } catch { native.ui.notify('AF wake configuration refused. No host message sent.', 'error'); }
    },
  });
  pi.registerCommand('af-wake-disconnect', { description: 'Stop this process-local AF wake hook',
    handler: async () => { stop(); } });
}
