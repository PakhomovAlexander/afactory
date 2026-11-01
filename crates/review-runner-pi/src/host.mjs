// AF's credential-bearing host, never a Worker input. Protocol: af.pi-host/1.
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';

export const PROTOCOL = 'af.pi-host/1';
export const VERSION = '1.4.0';
const MAX_LINE = 4 * 1024 * 1024;
const FILE_LIMIT = 1024 * 1024;
const RESULT_LIMIT = 64 * 1024;
class HostError extends Error {
  constructor(code) { super(code); this.code = code; }
}
const fail = (code) => { throw new HostError(code); };
const hash = (text) => 'sha256:' + createHash('sha256').update(text).digest('hex');
export function principal(credential) {
  if (credential?.type !== 'oauth' || typeof credential.accountId !== 'string' ||
      !credential.accountId.trim() || /[\x00-\x1f\x7f]/.test(credential.accountId) || credential.accountId.length > 512 ||
      typeof credential.access !== 'string' || !credential.access || typeof credential.refresh !== 'string' || !credential.refresh ||
      !Number.isFinite(credential.expires)) fail('unsupported_auth');
  // Pi's inference adapter extracts its routing account from this same access token.
  // Compare that non-secret claim locally; a stale accountId must not mask token replacement.
  let routedAccount;
  try {
    const parts = credential.access.split('.');
    if (parts.length !== 3 || parts[1].length > 128 * 1024) fail('unsupported_auth');
    const payload = JSON.parse(Buffer.from(parts[1], 'base64url').toString('utf8'));
    routedAccount = payload?.['https://api.openai.com/auth']?.chatgpt_account_id;
  } catch { fail('unsupported_auth'); }
  if (routedAccount !== credential.accountId) fail('identity_changed');
  return hash('af.pi.account/openai-codex\0' + credential.accountId);
}
// Every read and every refresh transaction checks the same source inference uses. Token
// rotation is allowed; a changed account is rejected before a refreshed credential can escape.
// AuthStorage.read resolves API-key helpers before returning. Inspect the raw credential
// through its supported locked modify callback, returning undefined (no write) instead.
export async function storedOAuth(storage, options) {
  return storage.modify('openai-codex', async (current) => { principal(current); return undefined; }, options);
}
export function guardedCredentials(storage, expected) {
  const check = (value) => {
    if (principal(value) !== expected) fail('identity_changed');
    return value;
  };
  return {
    async read(id, options) {
      if (id !== 'openai-codex') return undefined;
      return check(await storedOAuth(storage, options));
    },
    async list(options) { check(await storedOAuth(storage, options)); return [{ providerId: 'openai-codex', type: 'oauth' }]; },
    async modify(id, fn, options) {
      if (id !== 'openai-codex') fail('unsupported_backend');
      return check(await storage.modify(id, async (value) => {
        const next = await fn(check(value));
        if (next !== undefined) check(next);
        return next;
      }, options));
    },
    async delete() { fail('unsupported_auth'); },
  };
}
export function emptyResources(sdk) {
  const extensions = { extensions: [], errors: [], runtime: sdk.createExtensionRuntime() };
  return {
    getExtensions: () => extensions,
    getSkills: () => ({ skills: [], diagnostics: [] }),
    getPrompts: () => ({ prompts: [], diagnostics: [] }),
    getThemes: () => ({ themes: [], diagnostics: [] }),
    getAgentsFiles: () => ({ agentsFiles: [] }),
    getSystemPrompt: () => 'Follow only the AF Worker request. Use only the declared confined file tools.',
    getSystemPromptSource: () => undefined,
    getAppendSystemPrompt: () => [],
    getAppendSystemPromptSources: () => [],
    extendResources: () => fail('resource_injection'),
    reload: async () => {},
  };
}
// Reject symlinks (including in-tree links) and paths outside granted roots. Model-visible code cannot
// run concurrently with these synchronous operations: no shell, extensions or other tools.
export function confinedTools(cwd, readable, writable) {
  const aliases = [...readable, ...writable].map((value) => [path.resolve(value), fs.realpathSync(value)]);
  const roots = (values) => values.map((value) => fs.realpathSync(value));
  cwd = fs.realpathSync(cwd);
  readable = roots(readable); writable = roots(writable);
  const inside = (root, candidate) => candidate === root || candidate.startsWith(root + path.sep);
  function resolve(name, writing = false) {
    if (typeof name !== 'string' || !name || name.includes('\0')) fail('path_refused');
    let target = path.resolve(cwd, name);
    const alias = aliases.find(([spelling]) => inside(spelling, target));
    if (alias) target = path.join(alias[1], path.relative(alias[0], target));
    const root = (writing ? writable : readable).find((r) => inside(r, target));
    if (!root) fail('path_refused');
    let current = root;
    for (const part of path.relative(root, target).split(path.sep).filter(Boolean)) {
      current = path.join(current, part);
      try {
        const info = fs.lstatSync(current);
        if (info.isSymbolicLink()) fail('path_refused');
      } catch (error) {
        if (writing && error.code === 'ENOENT') continue;
        throw error;
      }
    }
    return target;
  }
  function read(name) {
    const target = resolve(name);
    const fd = fs.openSync(target, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW | fs.constants.O_NONBLOCK);
    try {
      const info = fs.fstatSync(fd);
      if (!info.isFile() || info.size > FILE_LIMIT) fail('file_limit');
      return fs.readFileSync(fd, 'utf8');
    } finally { fs.closeSync(fd); }
  }
  function write(name, content) {
    const target = resolve(name, true);
    if (typeof content !== 'string' || Buffer.byteLength(content) > FILE_LIMIT) fail('file_limit');
    // Create parents only beneath a writable grant, checking each component again before use.
    let parent = writable.find((root) => inside(root, target));
    if (target === parent) fail('path_refused');
    for (const part of path.relative(parent, path.dirname(target)).split(path.sep).filter(Boolean)) {
      parent = path.join(parent, part);
      try { fs.mkdirSync(parent, { mode: 0o700 }); }
      catch (error) { if (error.code !== 'EEXIST') throw error; }
      const info = fs.lstatSync(parent);
      if (!info.isDirectory() || info.isSymbolicLink()) fail('path_refused');
    }
    const fd = fs.openSync(target, fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_NOFOLLOW | fs.constants.O_NONBLOCK, 0o600);
    try {
      if (!fs.fstatSync(fd).isFile()) fail('path_refused');
      fs.ftruncateSync(fd, 0); fs.writeFileSync(fd, content);
    } finally { fs.closeSync(fd); }
  }
  function walk(name, recursive) {
    const start = resolve(name); const found = []; let visited = 0;
    function visit(dir) {
      for (const entry of fs.readdirSync(dir, { withFileTypes: true }).sort((a,b) => a.name.localeCompare(b.name))) {
        if (++visited > 10000) fail('file_limit');
        if (entry.isSymbolicLink()) continue;
        const file = path.join(dir, entry.name);
        resolve(file);
        if (entry.isFile()) found.push(file);
        else if (entry.isDirectory() && recursive) visit(file);
        else if (entry.isDirectory()) found.push(file + '/');
      }
    }
    if (fs.lstatSync(start).isDirectory()) visit(start); else found.push(start);
    return found;
  }
  const string = { type: 'string' };
  const specs = {
    read: [{ path: string, offset: { type: 'integer', minimum: 1 }, limit: { type: 'integer', minimum: 1, maximum: 2000 } }, ['path'], (a) => {
      const lines = read(a.path).split('\n');
      const start = (a.offset ?? 1) - 1; const limit = a.limit ?? 200;
      if (!Number.isSafeInteger(start) || start < 0 || !Number.isSafeInteger(limit) || limit < 1 || limit > 2000) fail('file_limit');
      const selected = lines.slice(start, start + limit).join('\n');
      return start + limit < lines.length ? `${selected}\n[More lines: read with offset ${start+limit+1}]` : selected;
    }],
    ls: [{ path: string }, ['path'], (a) => walk(a.path, false).join('\n')],
    find: [{ path: string, pattern: string }, ['path'], (a) => walk(a.path, true).filter((f) => !a.pattern || f.includes(a.pattern)).join('\n')],
    grep: [{ path: string, pattern: string }, ['path','pattern'], (a) => {
      if (!a.pattern || a.pattern.length > 256) fail('path_refused');
      // Literal search: no regular-expression backtracking or external program.
      const output = []; let bytes = 0;
      for (const file of walk(a.path, true)) {
        const lines = read(file).split('\n');
        for (let i = 0; i < lines.length; i++) if (lines[i].includes(a.pattern)) {
          const found = `${file}:${i+1}:${lines[i]}`;
          bytes += Buffer.byteLength(found) + 1;
          if (bytes > RESULT_LIMIT) fail('file_limit');
          output.push(found);
        }
      }
      return output.join('\n');
    }],
  };
  if (writable.length) {
    specs.write = [{ path: string, content: string }, ['path','content'], (a) => { write(a.path, a.content); return 'written'; }];
    specs.edit = [{ path: string, oldText: string, newText: string }, ['path','oldText','newText'], (a) => {
      resolve(a.path, true); const text = read(a.path);
      if (!a.oldText || !text.includes(a.oldText) || text.indexOf(a.oldText) !== text.lastIndexOf(a.oldText)) fail('edit_ambiguous');
      write(a.path, text.replace(a.oldText, () => a.newText)); return 'edited';
    }];
  }
  return Object.entries(specs).map(([name, [properties, required, execute]]) => ({
    name, label: name, description: `AF confined ${name}; paths must be within authorized roots.`,
    parameters: { type: 'object', properties, required, additionalProperties: false },
    async execute(_id, args, signal) {
      if (signal?.aborted) fail('cancelled');
      try {
        const text = execute(args);
        if (Buffer.byteLength(text) > RESULT_LIMIT) fail('file_limit');
        return { content: [{ type: 'text', text }], details: {} };
      } catch { throw new Error('AF file operation refused'); }
    },
  }));
}
export async function loadSdk(entry) {
  const real = fs.realpathSync(entry);
  const pkg = JSON.parse(fs.readFileSync(path.resolve(path.dirname(real), '../package.json'), 'utf8'));
  if (pkg.name !== '@earendil-works/pi-coding-agent' || pkg.version !== VERSION || path.basename(real) !== 'index.js') fail('sdk_version');
  for (const dependency of ['pi-ai','pi-agent-core']) {
    const meta = JSON.parse(fs.readFileSync(path.resolve(path.dirname(real), `../../${dependency}/package.json`), 'utf8'));
    if (meta.name !== `@earendil-works/${dependency}` || meta.version !== VERSION) fail('sdk_version');
  }
  const sdk = await import(pathToFileURL(real).href);
  const auth = await import(pathToFileURL(path.join(path.dirname(real), 'core/auth-storage.js')).href);
  const ai = await import(pathToFileURL(path.resolve(path.dirname(real), '../../pi-ai/dist/models.js')).href);
  return { ...sdk, AuthStorage: auth.AuthStorage, getSupportedThinkingLevels: ai.getSupportedThinkingLevels };
}
export async function createRuntime(sdk, credentials) {
  return sdk.ModelRuntime.create({ credentials, modelsPath: null, allowModelNetwork: false, refreshOnCreate: false });
}
const levels = ['off','minimal','low','medium','high','xhigh','max','ultra'];
function modelIdentity(model) {
  if (model.provider !== 'openai-codex' || model.api !== 'openai-codex-responses' || model.baseUrl !== 'https://chatgpt.com/backend-api') fail('model_changed');
  return hash('af.pi.model/1\0' + JSON.stringify(model));
}
export async function createSession(sdk, runtime, model, effort, cwd, readable, writable, agentDir) {
  const expectedModel = modelIdentity(model);
  const tools = confinedTools(cwd, readable, writable);
  const settings = sdk.SettingsManager.inMemory({
    compaction: { enabled: false }, retry: { enabled: false, provider: { maxRetries: 0 } }, cacheWarming: 'off',
    transport: 'sse',
  }, { projectTrusted: false });
  const result = await sdk.createAgentSession({ cwd, agentDir, modelRuntime: runtime, model,
    thinkingLevel: effort, settingsManager: settings, sessionManager: sdk.SessionManager.inMemory(cwd),
    resourceLoader: emptyResources(sdk), noTools: 'all', tools: tools.map((t) => t.name), customTools: tools,
  });
  const session = result.session;
  session.setAutoRetryEnabled(false); session.setAutoCompactionEnabled(false);
  let sameModel = false;
  try { sameModel = modelIdentity(session.model) === expectedModel; } catch { /* Refuse below and dispose. */ }
  if (result.modelFallbackMessage || !sameModel || session.thinkingLevel !== effort ||
      session.getActiveToolNames().sort().join(',') !== tools.map((t) => t.name).sort().join(',')) {
    session.dispose(); fail('settings_changed');
  }
  return { session, tools, settings };
}
function emit(type, data = {}) { process.stdout.write(JSON.stringify({ schema: PROTOCOL, type, ...data }) + '\n'); }
export async function main() {
  // No inherited NODE_OPTIONS/NODE_PATH is accepted by the Rust launcher. No credentials,
  // proxy, endpoint or cloud environment can become an alternate SDK authentication source.
  const major = Number(process.versions.node.split('.')[0]);
  if (major < 22 || (major === 22 && Number(process.versions.node.split('.')[1]) < 19)) fail('node_version');
  let buffer = Buffer.alloc(0); let initialized; let running = false; let session;
  for await (const chunk of process.stdin) {
    buffer = Buffer.concat([buffer, chunk]);
    if (buffer.length > MAX_LINE) fail('input_limit');
    let end;
    while ((end = buffer.indexOf(10)) >= 0) {
      const line = buffer.subarray(0,end); buffer = buffer.subarray(end+1);
      const request = JSON.parse(line.toString('utf8'));
      if (request.schema !== PROTOCOL) fail('protocol');
      if (!initialized && request.type === 'init') {
        if (request.backend !== 'openai-codex') fail('unsupported_backend');
        const sdk = await loadSdk(request.sdk);
        const storage = sdk.AuthStorage.create(path.join(request.auth_dir, 'auth.json'));
        const account = principal(await storedOAuth(storage));
        if (request.principal && request.principal !== account) fail('identity_changed');
        const credentials = guardedCredentials(storage, account);
        const runtime = await createRuntime(sdk, credentials);
        // Restrict authentication even if SDK defaults change: no API-key or env override.
        const stream = runtime.streamSimple.bind(runtime);
        runtime.streamSimple = (model, context, options) => {
          const inference = initialized.beginInference(model);
          return stream(model, context, { ...options, env: {}, maxRetries: 0,
            onProviderStreamEvent: (event, effective) => initialized.observe(inference, event, effective) });
        };
        const models = runtime.getModels('openai-codex').map((model) => ({ id: model.id, efforts: sdk.getSupportedThinkingLevels(model), identity: modelIdentity(model) }));
        initialized = { sdk, runtime, credentials, account, request };
        emit('ready', { principal: account, auth_method: 'oauth', backend: 'openai-codex', version: VERSION, models });
        if (request.probe) return;
      } else if (initialized && !running && request.type === 'prepare') {
        const state = initialized;
        const model = state.runtime.getModel('openai-codex', request.model);
        if (!model || modelIdentity(model) !== request.model_identity || !levels.includes(request.effort)) fail('model_changed');
        await state.credentials.read('openai-codex');
        const result = await createSession(state.sdk, state.runtime, model, request.effort,
          state.request.cwd, state.request.readable, state.request.writable, state.request.home);
        session = result.session;
        let inference = 0; let active; let failed = false; let settled = false; let finalText;
        initialized.beginInference = (effective) => {
          if ((active && !active.finished) || effective.provider !== 'openai-codex' || effective.id !== request.model || modelIdentity(effective) !== request.model_identity) {
            failed = true; fail('model_changed');
          }
          active = { id: ++inference, finished: false, observed: { input: null, output: null, cache_read: null, cache_write: null, reasoning: null } };
          finalText = undefined;
          emit('usage', { inference: active.id, complete: false, ...active.observed });
          return active;
        };
        initialized.observe = (current, event, effective) => {
          const u = event.response?.usage;
          let invalid = false;
          const counter = (value) => {
            if (value === undefined || value === null) return null;
            if (Number.isSafeInteger(value) && value >= 0) return value;
            invalid = true; return null;
          };
          if (u) {
            const next = { input: counter(u.input_tokens), output: counter(u.output_tokens),
              cache_read: counter(u.input_tokens_details?.cached_tokens),
              cache_write: counter(u.input_tokens_details?.cache_write_tokens),
              reasoning: counter(u.output_tokens_details?.reasoning_tokens) };
            for (const key of Object.keys(next)) if (next[key] !== null) current.observed[key] = next[key];
            emit('usage', { inference: current.id, complete: false, ...current.observed });
          }
          if (invalid) { failed = true; fail('invalid_usage'); }
          if (current.finished || effective.provider !== 'openai-codex' || effective.id !== request.model || modelIdentity(effective) !== request.model_identity) {
            failed = true; fail('model_changed');
          }
        };
        session.subscribe((event) => {
          if (['message_update','message_end'].includes(event.type) && event.message.role === 'assistant') {
            const msg = event.message;
            if (msg.provider !== 'openai-codex' || msg.model !== request.model) failed = true;
            if (event.type === 'message_end') {
              if (!active || active.finished) { failed = true; return; }
              active.finished = true;
              emit('usage', { inference: active.id, complete: !['error','aborted'].includes(msg.stopReason), ...active.observed });
            }
            if (event.type === 'message_end') {
              if (['error','aborted'].includes(msg.stopReason)) failed = true;
              if (msg.stopReason === 'stop') {
                const text = msg.content.filter((c) => c.type === 'text').map((c) => c.text).join('');
                if (Buffer.byteLength(text) > MAX_LINE / 2) failed = true;
                else finalText = text;
              }
            }
          }
          if (event.type === 'agent_settled') { settled = true; failed ||= event.aborted; }
        });
        initialized.run = async (text) => {
          await initialized.credentials.read('openai-codex');
          emit('accepted');
          try { await session.prompt(text, { expandPromptTemplates: false, source: 'rpc' }); } catch { failed = true; }
          if (!settled || failed || inference === 0 || finalText === undefined) emit('error', { code: 'inference_failed' });
          else { emit('message', { text: finalText }); emit('settled'); }
          session.dispose();
        };
        emit('prepared', { model: request.model, effort: request.effort, model_identity: request.model_identity });
      } else if (initialized?.run && !running && request.type === 'prompt' && typeof request.text === 'string') {
        running = true;
        await initialized.run(request.text);
        return;
      } else fail('protocol');
    }
  }
  session?.dispose();
  if (buffer.length || !initialized?.request.probe) fail('eof');
}
if (process.argv[1] && pathToFileURL(fs.realpathSync(process.argv[1])).href === import.meta.url) {
  main().catch((error) => {
    const safe = ['unsupported_auth','identity_changed','sdk_version','node_version','unsupported_backend','model_changed','settings_changed'];
    emit('error', { code: error instanceof HostError && safe.includes(error.code) ? error.code : 'host_refused' });
    process.exitCode = 1;
  });
}
