// AF's credential-bearing host, never a Worker input. Protocol: af.pi-host/4.
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';

export const PROTOCOL = 'af.pi-host/4';
export const VERSION = '1.4.0';
const MAX_LINE = 4 * 1024 * 1024;
const FILE_LIMIT = 1024 * 1024;
const RESULT_LIMIT = 64 * 1024;
class HostError extends Error {
  constructor(code) { super(code); this.code = code; }
}
const fail = (code) => { throw new HostError(code); };
const hash = (text) => 'sha256:' + createHash('sha256').update(text).digest('hex');
// Pi owns OAuth metadata. Validate only the required mode and credential fields; extra
// fields are opaque, never interpreted as identity, settings, routing or helpers. Reads
// bypass AuthStorage's helper resolution and refresh preserves the SDK's complete record.
function kimiIdentity(credential) {
  if (credential?.type !== 'oauth' || typeof credential.access !== 'string' || !credential.access ||
      typeof credential.refresh !== 'string' || !credential.refresh || !Number.isFinite(credential.expires)) fail('unsupported_auth');
  return null;
}
function openaiIdentity(credential) {
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
// Each admitted backend supplies credential identity, native model routing and raw usage
// accessors. Session orchestration, confinement, lifecycle and normalization stay common.
const BACKENDS = Object.freeze({
  'openai-codex': Object.freeze({
    identity: 'account', credentialIdentity: openaiIdentity,
    api: 'openai-codex-responses', endpoint: 'https://chatgpt.com/backend-api',
    usageSources: (event) => [event.response?.usage],
    usageFields: (u) => ({ input: u.input_tokens, output: u.output_tokens,
      cached: [u.input_tokens_details?.cached_tokens],
      cache_write: u.input_tokens_details?.cache_write_tokens,
      reasoning: u.output_tokens_details?.reasoning_tokens }),
  }),
  'kimi-coding': Object.freeze({
    identity: 'unavailable', credentialIdentity: kimiIdentity,
    api: 'openai-completions', endpoint: 'https://api.kimi.com/coding/v1', model: 'k3',
    usageSources: (event) => [event.usage, ...(event.choices ?? []).map((choice) => choice.usage)],
    usageFields: (u) => ({ input: u.prompt_tokens, output: u.completion_tokens,
      cached: [u.prompt_tokens_details?.cached_tokens, u.prompt_cache_hit_tokens, u.cached_tokens],
      cache_write: u.prompt_tokens_details?.cache_write_tokens,
      reasoning: u.completion_tokens_details?.reasoning_tokens }),
  }),
});
export function backendAdapter(backend) {
  if (!Object.hasOwn(BACKENDS, backend)) fail('unsupported_backend');
  return BACKENDS[backend];
}
export function principal(credential, backend = 'openai-codex') {
  return backendAdapter(backend).credentialIdentity(credential);
}
// Every read and refresh validates the same source inference uses. OpenAI account drift
// refuses; Kimi has no account identity to compare. Context and OAuth mode remain guarded.
// AuthStorage.read resolves API-key helpers before returning. Inspect the raw credential
// through its supported locked modify callback, returning undefined (no write) instead.
export async function storedOAuth(storage, options, backend = 'openai-codex') {
  backendAdapter(backend);
  return storage.modify(backend, async (current) => { principal(current, backend); return undefined; }, options);
}
export function guardedCredentials(storage, expected, backend = 'openai-codex', checkContext = () => {}) {
  const check = (value) => {
    checkContext();
    if (principal(value, backend) !== expected) fail('identity_changed');
    return value;
  };
  return {
    async read(id, options) {
      if (id !== backend) return undefined;
      checkContext();
      return check(await storedOAuth(storage, options, backend));
    },
    async list(options) { checkContext(); check(await storedOAuth(storage, options, backend)); return [{ providerId: backend, type: 'oauth' }]; },
    async modify(id, fn, options) {
      if (id !== backend) fail('unsupported_backend');
      checkContext();
      return check(await storage.modify(id, async (value) => {
        const next = await fn(check(value));
        if (next !== undefined) check(next);
        return next;
      }, options));
    },
    async delete() { fail('unsupported_auth'); },
  };
}
// Only raw transport usage is evidence. SDK AssistantMessage counters start at zero.
// A backend's cumulative observations replace counters for the same inference in Rust.
export function rawUsage(backend, event, onInvalid = () => fail('invalid_usage')) {
  let invalid = false;
  const adapter = backendAdapter(backend);
  const sources = adapter.usageSources(event);
  let result;
  const counter = (value) => {
    if (value === undefined || value === null) return null;
    if (!Number.isSafeInteger(value) || value < 0) { invalid = true; return null; }
    return value;
  };
  for (const u of sources) {
    if (u === undefined || u === null) continue;
    if (typeof u !== 'object' || Array.isArray(u)) fail('invalid_usage');
    const raw = adapter.usageFields(u);
    const observedCached = raw.cached.map(counter).filter((value) => value !== null);
    if (new Set(observedCached).size > 1) fail('invalid_usage');
    const next = { input: counter(raw.input),
      output: counter(raw.output),
      cache_read: observedCached[0] ?? null,
      cache_write: counter(raw.cache_write),
      reasoning: counter(raw.reasoning) };
    if (result && JSON.stringify(result) !== JSON.stringify(next)) fail('invalid_usage');
    result = next;
  }
  if (invalid) onInvalid();
  return result;
}
export const FILE_INSTRUCTIONS = 'Follow only the AF Worker request. Use only the declared confined file tools. You have no shell or check execution tool. Do not claim tests ran. For a typed Worker request, return exactly one JSON object with schema af.worker-reply/1 and outputs whose declared port values are ARRAYS of payloads matching output_schemas. No prose or Markdown fences. Example for a report port: {"schema":"af.worker-reply/1","outputs":{"report":[{"summary":"..."}]}}. Use the actual declared ports and schemas; the example does not add a report port. For other requests follow their exact reply contract (including admission OK).';
// The budget gates every SDK stream, not just session.prompt. Prompt cost is only known
// afterward: one inference may overshoot; its observed charge is retained in full.
export class InferenceBudget {
  constructor(tokens, backend) {
    if (!Number.isSafeInteger(tokens) || tokens <= 0) fail('invalid_allowance');
    backendAdapter(backend);
    this.tokens = BigInt(tokens); this.backend = backend; this.entries = [];
    this.unsafe = false;
  }
  begin(model) {
    if (this.unsafe) fail('unsafe_accounting');
    let charged = 0n;
    for (const entry of this.entries) {
      const u = entry.observed;
      if (!entry.finished || !entry.complete || u.input === null || u.output === null ||
          (this.backend === 'openai-codex' && u.cache_read === null)) fail('unsafe_accounting');
      charged += BigInt(u.input) - (this.backend === 'openai-codex' ? BigInt(u.cache_read) : 0n) + BigInt(u.output);
    }
    const remaining = this.tokens - charged;
    if (remaining <= 0n) fail('allowance_exhausted');
    // Stop before losing diagnostic coverage, rather than continuing with a partial trace.
    if (this.entries.length >= 256) fail('diagnostic_capacity');
    if (!Number.isSafeInteger(model.maxTokens) || model.maxTokens <= 0) fail('model_changed');
    const entry = { id: this.entries.length + 1, finished: false, complete: false,
      observations: 0, observed: { input: null, output: null, cache_read: null, cache_write: null, reasoning: null },
      maxTokens: Number(remaining < BigInt(model.maxTokens) ? remaining : BigInt(model.maxTokens)) };
    this.entries.push(entry); return entry;
  }
}
export function emptyResources(sdk, replySchema = null) {
  const extensions = { extensions: [], errors: [], runtime: sdk.createExtensionRuntime() };
  return {
    getExtensions: () => extensions,
    getSkills: () => ({ skills: [], diagnostics: [] }),
    getPrompts: () => ({ prompts: [], diagnostics: [] }),
    getThemes: () => ({ themes: [], diagnostics: [] }),
    getAgentsFiles: () => ({ agentsFiles: [] }),
    getSystemPrompt: () => FILE_INSTRUCTIONS + (replySchema === null ? '' : '\nActual final reply JSON Schema: ' + JSON.stringify(replySchema)),
    getSystemPromptSource: () => undefined,
    getAppendSystemPrompt: () => [],
    getAppendSystemPromptSources: () => [],
    extendResources: () => fail('resource_injection'),
    reload: async () => {},
  };
}
// Reject symlinks (including in-tree links) and paths outside granted roots. Model-visible code cannot
// run concurrently with these synchronous operations: no shell, extensions or other tools.
export function confinedTools(cwd, readable, writable, countTool = () => {}) {
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
        countTool(name);
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
export function modelIdentity(model) {
  const adapter = backendAdapter(model.provider);
  if (model.api !== adapter.api || model.baseUrl !== adapter.endpoint || (adapter.model && model.id !== adapter.model)) fail('model_changed');
  return hash('af.pi.model/1\0' + JSON.stringify(model));
}
export async function createSession(sdk, runtime, model, effort, cwd, readable, writable, agentDir, replySchema = null, countTool = () => {}) {
  const expectedModel = modelIdentity(model);
  const tools = confinedTools(cwd, readable, writable, countTool);
  const settings = sdk.SettingsManager.inMemory({
    compaction: { enabled: false }, retry: { enabled: false, provider: { maxRetries: 0 } }, cacheWarming: 'off',
    transport: 'sse',
  }, { projectTrusted: false });
  const result = await sdk.createAgentSession({ cwd, agentDir, modelRuntime: runtime, model,
    thinkingLevel: effort, settingsManager: settings, sessionManager: sdk.SessionManager.inMemory(cwd),
    resourceLoader: emptyResources(sdk, replySchema), noTools: 'all', tools: tools.map((t) => t.name), customTools: tools,
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
        backendAdapter(request.backend);
        const sdk = await loadSdk(request.sdk);
        const authContext = fs.realpathSync(request.auth_dir);
        const checkContext = () => {
          if (hash(fs.realpathSync(request.auth_dir)) !== request.auth_context_id ||
              fs.realpathSync(request.auth_dir) !== authContext || fs.lstatSync(request.auth_dir).isSymbolicLink()) fail('identity_changed');
        };
        checkContext();
        const storage = sdk.AuthStorage.create(path.join(request.auth_dir, 'auth.json'));
        const account = principal(await storedOAuth(storage, undefined, request.backend), request.backend);
        if (request.principal !== null && request.principal !== account) fail('identity_changed');
        const credentials = guardedCredentials(storage, account, request.backend, checkContext);
        const runtime = await createRuntime(sdk, credentials);
        // Restrict authentication even if SDK defaults change: no API-key or env override.
        const stream = runtime.streamSimple.bind(runtime);
        runtime.streamSimple = (model, context, options) => {
          const inference = initialized.beginInference(model);
          return stream(model, context, { ...options, maxTokens: inference.maxTokens, env: {}, maxRetries: 0,
            onProviderStreamEvent: (event, effective) => initialized.observe(inference, event, effective) });
        };
        const models = runtime.getModels(request.backend).filter((model) => !backendAdapter(request.backend).model || model.id === backendAdapter(request.backend).model).map((model) => ({ id: model.id, efforts: sdk.getSupportedThinkingLevels(model), identity: modelIdentity(model) }));
        initialized = { sdk, runtime, credentials, account, request };
        emit('ready', { principal: account, auth_method: 'oauth', backend: request.backend, version: VERSION, models });
        if (request.probe) return;
      } else if (initialized && !running && request.type === 'prepare') {
        const state = initialized;
        const budget = new InferenceBudget(request.remaining_tokens, state.request.backend);
        if (request.reply_schema !== null && (typeof request.reply_schema !== 'object' || Array.isArray(request.reply_schema))) fail('protocol');
        const toolCounts = {}; let toolCalls = 0;
        const countTool = (name) => {
          if (++toolCalls > 4096) { budget.unsafe = true; fail('diagnostic_capacity'); }
          toolCounts[name] = (toolCounts[name] ?? 0) + 1;
          emit('tool_count', { name, count: toolCounts[name] });
        };
        const model = state.runtime.getModel(state.request.backend, request.model);
        if (!model || modelIdentity(model) !== request.model_identity || !levels.includes(request.effort) || !state.sdk.getSupportedThinkingLevels(model).includes(request.effort)) fail('model_changed');
        await state.credentials.read(state.request.backend);
        const result = await createSession(state.sdk, state.runtime, model, request.effort,
          state.request.cwd, state.request.readable, state.request.writable, state.request.home, request.reply_schema, countTool);
        session = result.session;
        let inference = 0; let active; let failed = false; let settled = false; let finalText;
        initialized.beginInference = (effective) => {
          if ((active && !active.finished) || effective.provider !== state.request.backend || effective.id !== request.model || modelIdentity(effective) !== request.model_identity) {
            failed = true; fail('model_changed');
          }
          if (failed) fail('unsafe_accounting');
          active = budget.begin(effective); inference = active.id;
          finalText = undefined;
          emit('usage', { inference: active.id, complete: false, observations: active.observations, ...active.observed });
          return active;
        };
        initialized.observe = (current, event, effective) => {
          let next; let invalid = false;
          try { next = rawUsage(state.request.backend, event, () => { invalid = true; }); }
          catch { failed = true; fail('invalid_usage'); }
          if (next) {
            // Validate cumulative counters before allowing another inference. Preserve safe
            // observed floors even if another counter in this same event is malformed.
            for (const key of Object.keys(next)) if (next[key] !== null) {
              if (current.observed[key] !== null && next[key] < current.observed[key]) invalid = true;
              else current.observed[key] = next[key];
            }
            const u = current.observed;
            if ((u.input !== null && ((u.cache_read ?? 0) + (u.cache_write ?? 0) > u.input)) ||
                (u.output !== null && (u.reasoning ?? 0) > u.output)) invalid = true;
            current.observations++;
            emit('usage', { inference: current.id, complete: false, observations: current.observations, ...current.observed });
            if (current.observations >= 128) { budget.unsafe = true; fail('diagnostic_capacity'); }
          }
          if (invalid) { budget.unsafe = true; failed = true; fail('invalid_usage'); }
          if (current.finished || effective.provider !== state.request.backend || effective.id !== request.model || modelIdentity(effective) !== request.model_identity) {
            failed = true; fail('model_changed');
          }
        };
        session.subscribe((event) => {
          if (['message_update','message_end'].includes(event.type) && event.message.role === 'assistant') {
            const msg = event.message;
            if (msg.provider !== state.request.backend || msg.model !== request.model) failed = true;
            if (event.type === 'message_end') {
              if (!active || active.finished) { failed = true; return; }
              active.finished = true;
              active.complete = !failed && !['error','aborted'].includes(msg.stopReason);
              emit('usage', { inference: active.id, complete: active.complete, observations: active.observations, ...active.observed });
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
        initialized.run = async (text, promptNonce) => {
          await initialized.credentials.read(state.request.backend);
          emit('accepted', { prompt_nonce: promptNonce });
          try { await session.prompt(text, { expandPromptTemplates: false, source: 'rpc' }); } catch { failed = true; }
          if (!settled || failed || inference === 0 || finalText === undefined) emit('error', { code: 'inference_failed' });
          else { emit('message', { text: finalText }); emit('settled'); }
          session.dispose();
        };
        emit('prepared', { model: request.model, effort: request.effort, model_identity: request.model_identity });
      } else if (initialized?.run && !running && request.type === 'prompt' && typeof request.text === 'string') {
        if (typeof request.prompt_nonce !== 'string' || !/^sha256:[0-9a-f]{64}$/.test(request.prompt_nonce)) fail('protocol');
        running = true;
        await initialized.run(request.text, request.prompt_nonce);
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
