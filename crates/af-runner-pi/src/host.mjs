// AF's credential-bearing host, never a Worker input. Protocol: af.pi-host/5.
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';

export const PROTOCOL = 'af.pi-host/5';
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
function unavailableOAuthIdentity(credential) {
  if (credential?.type !== 'oauth' || typeof credential.access !== 'string' || !credential.access ||
      typeof credential.refresh !== 'string' || (!credential.refresh && credential.expires !== Number.MAX_SAFE_INTEGER) || !Number.isFinite(credential.expires)) fail('unsupported_auth');
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
// Identity is available only where the native credential supplies a checked routing account.
// Other SDK auth categories carry no fabricated account principal.
export function principal(credential, backend = 'openai-codex') {
  if (backend === 'openai-codex') return openaiIdentity(credential);
  if (credential === undefined) return null; // explicitly selected environment/config auth
  if (credential?.type === 'oauth') return unavailableOAuthIdentity(credential);
  if (credential?.type === 'api_key' && typeof credential.key === 'string' && credential.key.startsWith('!')) fail('helper_authority');
  if (credential?.type === 'api_key' &&
      (credential.key === undefined || (typeof credential.key === 'string' && credential.key && !credential.key.startsWith('!'))) &&
      (credential.env === undefined || (credential.env && typeof credential.env === 'object' &&
       !Array.isArray(credential.env) && Object.values(credential.env).every(v => typeof v === 'string')))) return null;
  fail('unsupported_auth');
}
// Inspect without executing AuthStorage's command-capable read method.
export async function storedCredential(storage, options, backend = 'openai-codex') {
  return storage.modify(backend, async (current) => { principal(current, backend); return undefined; }, options);
}
export function guardedCredentials(storage, expected, backend = 'openai-codex', checkContext = () => {}, initialCredential) {
  let mode = arguments.length >= 5 ? initialCredential?.type ?? 'context' : undefined;
  let keyContext = initialCredential?.type === 'api_key' ? JSON.stringify(initialCredential) : undefined;
  const check = (value) => {
    checkContext();
    const next = value?.type ?? 'context';
    if (mode !== undefined && mode !== next) fail('identity_changed');
    mode = next;
    if (value?.type === 'api_key') {
      const current = JSON.stringify(value);
      if (keyContext !== undefined && current !== keyContext) fail('identity_changed');
      keyContext = current;
    }
    if (principal(value, backend) !== expected) fail('identity_changed');
    return value;
  };
  return {
    async read(id, options) {
      if (id !== backend) return undefined;
      return check(await storedCredential(storage, options, backend));
    },
    async list(options) {
      const value = check(await storedCredential(storage, options, backend));
      return value ? [{ providerId: backend, type: value.type }] : [];
    },
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
// This is a transport accounting map, never a provider/model allowlist. Providers and
// models are resolved by ModelRuntime. Each raw wire contract is normalized to total
// input/output; optional components follow their native wire contract, never initialized
// AssistantMessage counters.
const completion = {
  usageSources: (e, object, invalidate) => {
    const sources = [e.usage];
    if (e.choices != null) {
      if (!Array.isArray(e.choices)) invalidate();
      else for (const c of e.choices) { if (c == null) invalidate(); else sources.push(object(c)?.usage); }
    }
    return sources;
  },
  usageFields: (u, object) => ({ input: u.prompt_tokens, output: u.completion_tokens,
    cached: [object(u.prompt_tokens_details)?.cached_tokens, object(u.prompt_token_details)?.cached_tokens, object(u.promptTokensDetails)?.cachedTokens,
      object(u.promptTokenDetails)?.cachedTokens, u.prompt_cache_hit_tokens, u.cached_tokens, u.num_cached_tokens, u.numCachedTokens],
    cache_write: object(u.prompt_tokens_details)?.cache_write_tokens,
    reasoning: object(u.completion_tokens_details)?.reasoning_tokens }),
};
const responses = {
  usageSources: (e, object) => [object(e.response)?.usage],
  usageFields: (u, object) => ({ input: u.input_tokens, output: u.output_tokens,
    cached: [object(u.input_tokens_details)?.cached_tokens],
    cache_write: object(u.input_tokens_details)?.cache_write_tokens,
    reasoning: object(u.output_tokens_details)?.reasoning_tokens }),
};
const add = (values, invalidate) => {
  if (values.every(v => v == null)) return undefined;
  if (values.some(v => !Number.isSafeInteger(v) || v < 0)) invalidate();
  // Retain observed component floors even when completeness is unsafe.
  return values.filter(v => Number.isSafeInteger(v) && v >= 0).reduce((a,b) => a+b,0);
};
// Anthropic initializes optional cache components to zero at message_start and updates
// only non-null delta fields. Normalize only actual native usage, never SDK assistant
// defaults; mandatory input/output still require native evidence. Retain component floors
// on malformed/decreasing updates, including in the reported cache breakdown.
function anthropicInput(usage, components, invalidate) {
  const names = ['input_tokens','cache_read_input_tokens','cache_creation_input_tokens'];
  for (const name of names.slice(1)) components[name] ??= 0;
  for (const name of names) {
    const value = usage[name];
    if (value == null) continue;
    if (!Number.isSafeInteger(value) || value < 0 ||
        (components[name] !== undefined && value < components[name])) invalidate();
    else components[name] = value;
  }
  const values = names.map(name => components[name]);
  let total = add(values,invalidate);
  if (total !== undefined && !Number.isSafeInteger(total)) {
    invalidate();
    // An unrepresentable sum must still retain a representable observed charge floor.
    total = Math.max(...values.filter(v => Number.isSafeInteger(v) && v >= 0));
  }
  // Optional defaults alone are not an observation of mandatory input. Keep a positive
  // cache floor on invalid incomplete usage, but never fabricate zero mandatory usage.
  return components.input_tokens === undefined && total === 0 ? undefined : total;
}
export const TRANSPORTS = Object.freeze({
  'openai-completions': completion, 'mistral-conversations': completion,
  'openai-responses': responses, 'openai-codex-responses': responses, 'azure-openai-responses': responses,
  'anthropic-messages': {
    usageSources: (e, object) => [object(e.message)?.usage, e.usage],
    usageFields: (u, object, invalidate, components) => ({ input: anthropicInput(u,components,invalidate),
      output: u.output_tokens, cached: [components.cache_read_input_tokens], cache_write: components.cache_creation_input_tokens,
      reasoning: object(u.output_tokens_details)?.thinking_tokens }),
  },
  'google-generative-ai': {
    usageSources: e => [e.usageMetadata],
    usageFields: (u,object,invalidate) => ({ input: u.promptTokenCount,
      output: Number.isSafeInteger(u.totalTokenCount) && Number.isSafeInteger(u.promptTokenCount)
        ? u.totalTokenCount-u.promptTokenCount : add([u.candidatesTokenCount,u.thoughtsTokenCount],invalidate),
      cached: [u.cachedContentTokenCount], reasoning: u.thoughtsTokenCount }),
  },
  'google-vertex': null,
  'bedrock-converse-stream': {
    usageSources: (e,object) => [object(e.metadata)?.usage],
    usageFields: u => ({ input: u.inputTokens, output: u.outputTokens,
      cached: [u.cacheReadInputTokens], cache_write: u.cacheWriteInputTokens }),
  },
  'pi-messages': {
    usageSources: e => [e.usage],
    usageFields: (u,object,invalidate) => ({ input: add([u.input,u.cacheRead,u.cacheWrite],invalidate), output: u.output,
      cached: [u.cacheRead], cache_write: u.cacheWrite, reasoning: u.reasoning }),
  },
});
export function transportAdapter(api) {
  if (api === 'google-vertex') api = 'google-generative-ai';
  const adapter = Object.hasOwn(TRANSPORTS,api) ? TRANSPORTS[api] : null;
  if (!adapter) fail('unsupported_transport');
  return adapter;
}
// Only raw transport usage is evidence. SDK AssistantMessage counters start at zero.
// A backend's cumulative observations replace counters for the same inference in Rust.
export function rawUsage(backend, event, onInvalid = () => fail('invalid_usage'), components = {}) {
  let invalid = false;
  const adapter = transportAdapter(backend);
  const invalidate = () => { invalid = true; };
  // Only the finite recognized native containers are inspected. Optional null/absent
  // containers mean unknown; other non-object shapes fail without losing sibling usage.
  const object = (value) => {
    if (value == null) return undefined;
    if (typeof value !== 'object' || Array.isArray(value)) { invalidate(); return undefined; }
    return value;
  };
  const native = object(event);
  if (!native) { onInvalid(); return undefined; }
  const sources = adapter.usageSources(native, object, invalidate);
  let result;
  const counter = (value) => {
    if (value === undefined || value === null) return null;
    if (!Number.isSafeInteger(value) || value < 0) { invalid = true; return null; }
    return value;
  };
  const keys = ['input', 'output', 'cache_read', 'cache_write', 'reasoning'];
  const conflicted = new Set();
  for (const u of sources) {
    const source = object(u);
    if (!source) continue;
    const raw = adapter.usageFields(source, object, invalidate, components);
    // Malformed aliases fail accounting but do not hide independently valid numbers.
    // Only distinct numeric aliases conflict; keep that conflict permanent across sources.
    const cached = raw.cached.map(counter);
    const observedCached = cached.filter((value) => value !== null);
    const cacheConflict = new Set(observedCached).size > 1;
    if (cacheConflict) conflicted.add('cache_read');
    if (cacheConflict) invalid = true;
    const next = { input: counter(raw.input), output: counter(raw.output),
      cache_read: cacheConflict ? null : observedCached[0] ?? null,
      cache_write: counter(raw.cache_write), reasoning: counter(raw.reasoning) };
    if (!result) result = next;
    else for (const key of keys) {
      // Unknown is absence, not a value. Only distinct observed numbers conflict;
      // that state is permanent for this event, regardless of later sources.
      if (result[key] !== null && next[key] !== null && result[key] !== next[key]) {
        invalid = true; conflicted.add(key);
      }
      if (conflicted.has(key)) result[key] = null;
      else if (next[key] !== null) result[key] = next[key];
    }
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
      observations: 0, nativeComponents: {}, observed: { input: null, output: null, cache_read: null, cache_write: null, reasoning: null },
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
// Only files in the explicitly selected private profile are configuration inputs. No
// candidate discovery, helper interpretation or ambient process environment is involved.
function boundedBytes(fd, size, code) {
  const buffer = Buffer.alloc(size+1); let count = 0;
  while (count < buffer.length) {
    const read = fs.readSync(fd,buffer,count,buffer.length-count,null);
    if (!read) break;
    count += read;
  }
  if (count > size) fail(code);
  return buffer.subarray(0,count);
}
function extensionBytes(extension) {
  try {
    if (!extension || Object.keys(extension).sort().join(',') !== 'entrypoint,sha256' ||
        !/^sha256:[0-9a-f]{64}$/.test(extension.sha256)) fail('extension_authority');
    const entry = fs.realpathSync(extension.entrypoint);
    if (entry !== extension.entrypoint || !path.isAbsolute(entry)) fail('extension_authority');
    for (let parent = path.dirname(entry);;) {
      if (fs.existsSync(path.join(parent,'.git')) || fs.existsSync(path.join(parent,'.af'))) fail('extension_authority');
      const next = path.dirname(parent); if (next === parent) break; parent = next;
    }
    const fd = fs.openSync(entry,fs.constants.O_RDONLY|fs.constants.O_NOFOLLOW|fs.constants.O_NONBLOCK);
    try {
      const stat = fs.fstatSync(fd);
      if (!stat.isFile() || stat.size > MAX_LINE) fail('extension_authority');
      const bytes = boundedBytes(fd,stat.size,'extension_authority');
      if (hash(bytes) !== extension.sha256) fail('extension_authority');
      return bytes;
    } finally { fs.closeSync(fd); }
  } catch { fail('extension_authority'); }
}
function profileFile(dir, name, binary = false) {
  const file = path.join(dir,name);
  try {
    const fd = fs.openSync(file,fs.constants.O_RDONLY|fs.constants.O_NOFOLLOW|fs.constants.O_NONBLOCK);
    try {
      const stat = fs.fstatSync(fd);
      if (!stat.isFile() || stat.size > MAX_LINE || (stat.mode & 0o077) !== 0 || stat.uid !== process.getuid()) fail('profile_authority');
      const bytes = boundedBytes(fd,stat.size,'profile_authority');
      return binary ? bytes : bytes.toString('utf8');
    } finally { fs.closeSync(fd); }
  } catch (e) { if (e.code === 'ENOENT') return undefined; fail('profile_authority'); }
}
function profileObject(text) {
  try {
    const value = JSON.parse(text);
    if (!value || typeof value !== 'object' || Array.isArray(value)) fail('profile_authority');
    return value;
  } catch { fail('profile_authority'); }
}
function noCommands(value) {
  if (typeof value === 'string' && value.startsWith('!')) fail('helper_authority');
  if (value && typeof value === 'object') for (const child of Object.values(value)) noCommands(child);
}
function publicConfig(value) {
  if (Array.isArray(value)) return value.map(publicConfig);
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([k,v]) =>
    [k, ['environment','env'].includes(k) ? publicEnvironment(v) : ['apiKey','key','headers'].includes(k) ? Object.keys(v && typeof v === 'object' ? v : {}).sort() : publicConfig(v)]));
  return value;
}
function publicEnvironment(env) {
  return Object.fromEntries(Object.entries(env ?? {}).sort(([a],[b]) => a.localeCompare(b)).map(([k,v]) =>
    [k, /(?:REGION|LOCATION|PROJECT|PROFILE|ENDPOINT|BASE_URL|_FILE|CREDENTIALS|VERSION|DEPLOYMENT(?:_NAME)?(?:_MAP)?|RESOURCE_NAME|HOST|PORT|MODE|FORMAT|ENABLED|DISABLED|SKIP_AUTH)$/.test(k) && !/(?:SECRET|TOKEN|API_KEY|PASSWORD)/.test(k) ? v : true]));
}
function explicitEnvironment(env) {
  if (!env || typeof env !== 'object' || Array.isArray(env) || Object.entries(env).some(([k,v]) =>
      !/^[A-Z][A-Z0-9_]*$/.test(k) || /^(NODE_|PI_|HOME$|XDG_|PATH$|LD_|DYLD_)/.test(k) || typeof v !== 'string')) fail('profile_authority');
  const files = new Map(); const selectors = []; let bytes = 0;
  const captureFile = value => {
    if (!path.isAbsolute(value) || fs.realpathSync(value) !== value) fail('profile_authority');
    if (files.has(value)) return;
    for (let parent = path.dirname(value);;) {
      if (fs.existsSync(path.join(parent,'.git')) || fs.existsSync(path.join(parent,'.af'))) fail('profile_authority');
      const next = path.dirname(parent); if (next === parent) break; parent = next;
    }
    const raw = profileFile(path.dirname(value),path.basename(value),true);
    if (raw === undefined || files.size >= 32 || (bytes += raw.length) > MAX_LINE) fail('profile_authority');
    files.set(value,raw);
    const text = raw.toString('utf8');
    selectors.push([value]);
    // SDK credential chains may follow explicit ADC/INI file references. Capture that
    // closure too; a reference into the candidate is never credential authority.
    const scan = (object, parent = '') => {
      if (!object || typeof object !== 'object') return;
      for (let [key,child] of Object.entries(object)) {
        if (key === 'executable' || key === 'credential_process') fail('helper_authority');
        if (typeof child === 'string' && (key.endsWith('_file') || (parent === 'credential_source' && key === 'file'))) { selectors.push([value,parent,key,child]); captureFile(child); }
        else if (typeof child === 'string' && ['type','url','token_url','audience','subject_token_type','service_account_impersonation_url','quota_project_id','universe_domain'].includes(key)) {
          if (key === 'url' || key.endsWith('_url')) { const url = new URL(child); if (url.username || url.password) fail('profile_authority');
            for (const key of url.searchParams.keys()) if (/token|key|secret|signature|password|auth/i.test(key)) url.searchParams.set(key,'<credential>');
            child = url.href; }
          selectors.push([value,parent,key,child]);
        }
        else scan(child,key);
      }
    };
    if (text.trimStart().startsWith('{')) {
      try { scan(JSON.parse(text)); } catch (error) { if (error instanceof HostError) throw error; fail('profile_authority'); }
    } else {
      if (/^\s*credential_process\s*=/im.test(text)) fail('helper_authority');
      for (const line of text.split('\n')) {
        const match = /^\s*[a-z_]*_file\s*=\s*(.*?)\s*$/i.exec(line);
        if (match) { const file = match[1].replace(/^["']|["']$/g,''); selectors.push([value,file]); captureFile(file); }
        const setting = /^\s*(region|role_arn|source_profile|credential_source|sso_start_url|sso_region|sso_session)\s*=\s*(.*?)\s*$/i.exec(line);
        if (setting) selectors.push([value,setting[1],setting[2]]);
      }
    }
  };
  for (const [name,value] of Object.entries(env)) {
    if (/(?:_FILE|GOOGLE_APPLICATION_CREDENTIALS|AWS_CA_BUNDLE)$/.test(name)) captureFile(value);
  }
  if (env.AWS_PROFILE && !env.AWS_CONFIG_FILE && !env.AWS_SHARED_CREDENTIALS_FILE) fail('profile_authority');
  const check = () => {
    for (const [file,raw] of files) {
      const current = profileFile(path.dirname(file),path.basename(file),true);
      if (fs.realpathSync(file) !== file || !current?.equals(raw)) fail('configuration_changed');
    }
  };
  check.identity = hash('af.pi.credential-selectors/1\0'+JSON.stringify(selectors));
  return check;
}
export function loadProfile(dir, backend) {
  for (let parent = fs.realpathSync(dir);;) {
    if (fs.existsSync(path.join(parent,'.git')) || fs.existsSync(path.join(parent,'.af'))) fail('profile_authority');
    const next = path.dirname(parent); if (next === parent) break; parent = next;
  }
  const contextText = profileFile(dir,'af-context.json',true);
  const context = contextText === undefined ? {schema:'af.pi-context/1'} : profileObject(contextText);
  if (context.schema !== 'af.pi-context/1' || Object.keys(context).some(k => !['schema','environment','extension'].includes(k))) fail('profile_authority');
  const env = context.environment ?? {};
  const checkEnvironment = explicitEnvironment(env);
  const modelsText = profileFile(dir,'models.json',true);
  const models = modelsText === undefined ? {} : profileObject(modelsText);
  if (models.providers !== undefined && (!models.providers || typeof models.providers !== 'object' || Array.isArray(models.providers))) fail('profile_authority');
  const config = Object.hasOwn(models.providers ?? {},backend) ? models.providers[backend] : undefined;
  noCommands(config);
  const cacheText = profileFile(dir,'models-store.json',true);
  const cache = cacheText === undefined ? undefined : profileObject(cacheText)[backend];
  noCommands(cache);
  const extension = context.extension;
  if (extension) extensionBytes(extension);
  const sameFile = (name, previous) => { const current = profileFile(dir,name,true); return current === undefined ? previous === undefined : previous !== undefined && current.equals(previous); };
  return { env, config, cache, extension,
    identity: hash('af.pi.profile/1\0'+JSON.stringify(publicConfig({config,cache,extension,environment:env,credential_selectors:checkEnvironment.identity}))),
    // Comparison stays in memory: never hash or serialize credentials as authority/principals.
    check: () => {
      checkEnvironment();
      if (!sameFile('af-context.json',contextText) || !sameFile('models.json',modelsText) ||
          !sameFile('models-store.json',cacheText)) fail('configuration_changed');
      if (extension) extensionBytes(extension);
    } };
}
export async function createRuntime(sdk, credentials, profile = null, backend = null, home = null) {
  let modelsPath = null;
  if (profile?.config) {
    modelsPath = path.join(home,'models.json');
    fs.writeFileSync(modelsPath,JSON.stringify({providers:{[backend]:profile.config}}),{mode:0o600});
  }
  const store = { read: async id => id === backend ? structuredClone(profile?.cache) : undefined,
    write: async () => {}, delete: async () => {} };
  const runtime = await sdk.ModelRuntime.create({ credentials, modelsPath, modelsStore:store,
    allowModelNetwork: false, refreshOnCreate: false });
  if (profile?.extension) {
    let extension;
    try {
      // Import the verified bundle bytes, not a path that could change after hashing.
      // Relative/bare imports require bundling; there is no project module resolution.
      extension = await import('data:text/javascript;base64,'+extensionBytes(profile.extension).toString('base64'));
    } catch { fail('extension_authority'); }
    if (typeof extension.register !== 'function') fail('extension_authority');
    try { await extension.register(Object.freeze({
      registerProvider: (id,config) => { if (id !== backend) fail('extension_authority'); noCommands(config); runtime.registerProvider(id,config); },
      registerNativeProvider: provider => { if (provider.id !== backend) fail('extension_authority'); runtime.registerNativeProvider(provider); },
    })); } catch { fail('extension_authority'); }
  }
  if (profile) {
    if (!runtime.getProviders().some(p => p.id === backend)) fail('provider_configuration');
    const refresh = await runtime.refresh({allowNetwork:false,providers:[backend]});
    if (refresh.errors.size || runtime.getError()) fail('provider_configuration');
  }
  return runtime;
}
const levels = ['off','minimal','low','medium','high','xhigh','max'];
function guardSampling(value) {
  if (!value || typeof value !== 'object') return;
  const reserved = new Set(['model','messages','input','instructions','system','tools','functions','toolchoice','toolconfig',
    'builtintools','maxtokens','maxcompletiontokens','maxoutputtokens','stream','streamoptions','store','background',
    'previousresponseid','conversation','conversationid','session','sessionid','threadid','history','context',
    'endpoint','baseurl','headers','apikey','env']);
  for (const [key,child] of Object.entries(value)) {
    if (reserved.has(key.toLowerCase().replaceAll('_','').replaceAll('-',''))) fail('unsafe_model_configuration');
    guardSampling(child);
  }
}
// Account-qualified resource ARNs stay inside the credential-bearing host. The public
// selector pins that exact SDK ID without publishing its account component or claiming a principal.
export function publicModelId(model) {
  if (typeof model?.id !== 'string' || typeof model.provider !== 'string') fail('model_changed');
  return (model.id.startsWith('arn:') && Boolean(model.id.split(':')[4])) || model.id.startsWith('af-private-model/')
    ? 'af-private-model/'+hash('af.pi.private-model/1\0'+model.provider+'\0'+model.id).slice(7) : model.id;
}
export function modelIdentity(model, context = '') {
  guardSampling(model?.samplingParams); guardSampling(model?.samplingParamsByThinkingLevel);
  if (!model || (model.type !== undefined && model.type !== 'chat') || !model.api || !model.provider || !model.id) fail('model_changed');
  if (typeof model.baseUrl !== 'string') fail('model_changed');
  let url;
  try { url = model.baseUrl ? new URL(model.baseUrl) : null; } catch { fail('model_changed'); }
  if (url && (!['https:','http:'].includes(url.protocol) || url.username || url.password || url.search || url.hash)) fail('model_changed');
  return hash('af.pi.model/2\0' + context + '\0' + JSON.stringify(publicConfig(model)));
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

        const sdk = await loadSdk(request.sdk);
        const authContext = fs.realpathSync(request.auth_dir);
        const checkContext = () => {
          if (hash(fs.realpathSync(request.auth_dir)) !== request.auth_context_id ||
              fs.realpathSync(request.auth_dir) !== authContext || fs.lstatSync(request.auth_dir).isSymbolicLink()) fail('identity_changed');
        };
        checkContext();
        const profile = loadProfile(request.auth_dir,request.backend);
        const storage = sdk.AuthStorage.create(path.join(request.auth_dir, 'auth.json'));
        const rawCredential = await storedCredential(storage, undefined, request.backend);
        const account = principal(rawCredential, request.backend);
        const authMethod = rawCredential?.type ?? 'context';
        const selectedEnv = {...(rawCredential?.env ?? {}),...profile.env};
        const checkCredentialFiles = explicitEnvironment(selectedEnv);
        const verifyContext = () => { checkContext(); profile.check(); checkCredentialFiles(); };
        // Some official cloud transports use their SDK's process-level credential chain.
        // Only the explicitly selected context is exported into this already empty environment.
        if (selectedEnv.AWS_MAX_ATTEMPTS !== undefined && selectedEnv.AWS_MAX_ATTEMPTS !== '1') fail('settings_changed');
        Object.assign(process.env,selectedEnv);
        // Bedrock's AWS SDK owns an inner retry chain independently of Pi maxRetries.
        // Pin its initial request only, including for explicitly selected cloud contexts.
        process.env.AWS_MAX_ATTEMPTS = '1';
        if (request.principal !== null && request.principal !== account) fail('identity_changed');
        const credentials = guardedCredentials(storage, account, request.backend, verifyContext, rawCredential);
        let runtime;
        try { runtime = await createRuntime(sdk, credentials, profile, request.backend, request.home); }
        catch (error) { if (error instanceof HostError) throw error; fail('provider_configuration'); }
        const provider = runtime.getProviders().find(p => p.id === request.backend);
        if (rawCredential?.type === 'oauth' ? !provider?.auth?.oauth : !await runtime.getAuth(request.backend,{env:selectedEnv})) fail('missing_credentials');
        // Resolve only the selected context; OAuth refresh is deferred until prepare.
        const requestAuth = rawCredential?.type === 'oauth'
          ? {auth:await provider.auth.oauth.toAuth(rawCredential)} : await runtime.getAuth(request.backend,{env:selectedEnv});
        const effectiveIdentity = (model, resolved = false) => modelIdentity(!resolved && requestAuth?.auth?.baseUrl ? {...model,baseUrl:requestAuth.auth.baseUrl} : model,
          profile.identity + checkCredentialFiles.identity + hash(JSON.stringify(publicEnvironment(rawCredential?.env))));
        const checkAuth = async model => {
          verifyContext();
          const current = await runtime.getAuth(model ?? request.backend,{env:selectedEnv});
          if (!current) fail('missing_credentials');
          if ((current.auth.baseUrl ?? null) !== (requestAuth?.auth?.baseUrl ?? null)) fail('configuration_changed');
        };
        const stream = runtime.streamSimple.bind(runtime);
        runtime.streamSimple = (model, context, options) => {
          verifyContext();
          const inference = initialized.beginInference(model);
          return stream(model, context, { ...options, maxTokens: inference.maxTokens, env: selectedEnv, maxRetries: 0,
            onProviderStreamEvent: (event, effective) => initialized.observe(inference, event, effective) });
        };
        const catalog = runtime.getModels(request.backend).filter(model => model.type === undefined || model.type === 'chat');
        const models = catalog.map((model) => ({ id: publicModelId(model), efforts: sdk.getSupportedThinkingLevels(model), identity: effectiveIdentity(model) }));
        initialized = { sdk, runtime, credentials, account, request, profile, verifyContext, effectiveIdentity, checkAuth, catalog };
        if (!models.length) fail('no_chat_models');
        const offset = request.catalog_offset ?? 0;
        if (!Number.isSafeInteger(offset) || offset < 0 || offset >= 4096) fail('protocol');
        const selected = request.model === undefined ? models : models.filter(m => m.id === request.model);
        const page = selected.slice(offset,offset+64);
        emit('ready', { catalog_id:hash('af.pi.catalog/1\0'+JSON.stringify(models)), principal: account, auth_method: authMethod, backend: request.backend, version: VERSION, models:page,
          ...(offset+page.length < selected.length ? {next_offset:offset+page.length} : {}) });
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
        const selected = state.catalog.find(model => publicModelId(model) === request.model);
        const model = selected && state.runtime.getModel(state.request.backend, selected.id);
        if (!model || state.effectiveIdentity(model) !== request.model_identity || !levels.includes(request.effort) || !state.sdk.getSupportedThinkingLevels(model).includes(request.effort)) fail('model_changed');
        state.verifyContext();
        await state.credentials.read(state.request.backend);
        await state.checkAuth(model);
        transportAdapter(model.api);
        const result = await createSession(state.sdk, state.runtime, model, request.effort,
          state.request.cwd, state.request.readable, state.request.writable, state.request.home, request.reply_schema, countTool);
        session = result.session;
        let inference = 0; let active; let failed = false; let settled = false; let finalText;
        initialized.beginInference = (effective) => {
          if ((active && !active.finished) || effective.provider !== state.request.backend || effective.id !== model.id || state.effectiveIdentity(effective) !== request.model_identity) {
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
          try { next = rawUsage(effective.api, event, () => { invalid = true; }, current.nativeComponents); }
          catch { failed = true; fail('invalid_usage'); }
          if (next) {
            // Validate cumulative counters before allowing another inference. Preserve safe
            // observed floors even if another counter in this same event is malformed.
            const prior = current.observed;
            const safe = { ...prior };
            for (const key of Object.keys(next)) if (next[key] !== null) {
              if (prior[key] !== null && next[key] < prior[key]) invalid = true;
              else safe[key] = next[key];
            }
            // Do not send Rust an inconsistent record: it correctly rejects the entire
            // record. Roll back optional components first, retaining prior safe evidence.
            // If a newly known aggregate contradicts prior components, leave it unknown.
            const cacheExceeds = () => safe.input !== null &&
              BigInt(safe.cache_read ?? 0) + BigInt(safe.cache_write ?? 0) > BigInt(safe.input);
            if (cacheExceeds()) {
              invalid = true;
              safe.cache_read = prior.cache_read; safe.cache_write = prior.cache_write;
              if (cacheExceeds()) safe.input = prior.input;
            }
            if (safe.output !== null && (safe.reasoning ?? 0) > safe.output) {
              invalid = true; safe.reasoning = prior.reasoning;
              if ((safe.reasoning ?? 0) > safe.output) safe.output = prior.output;
            }
            current.observed = safe;
            current.observations++;
            emit('usage', { inference: current.id, complete: false, observations: current.observations, ...current.observed });
            if (current.observations >= 128) { budget.unsafe = true; fail('diagnostic_capacity'); }
          }
          if (invalid) { budget.unsafe = true; failed = true; fail('invalid_usage'); }
          if (current.finished || effective.provider !== state.request.backend || effective.id !== model.id || state.effectiveIdentity(effective,true) !== request.model_identity) {
            failed = true; fail('model_changed');
          }
        };
        session.subscribe((event) => {
          if (['message_update','message_end'].includes(event.type) && event.message.role === 'assistant') {
            const msg = event.message;
            if (msg.provider !== state.request.backend || msg.model !== model.id) failed = true;
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
          state.verifyContext();
          await initialized.credentials.read(state.request.backend);
          await state.checkAuth(model);
          emit('accepted', { prompt_nonce: promptNonce });
          try { await session.prompt(text, { expandPromptTemplates: false, source: 'rpc' }); } catch { failed = true; }
          const accounted = budget.entries.every(entry => entry.finished && entry.complete &&
            entry.observations > 0 && entry.observed.input !== null && entry.observed.output !== null &&
            (state.request.backend !== 'openai-codex' || entry.observed.cache_read !== null));
          if (!settled || failed || !accounted || inference === 0 || finalText === undefined) emit('error', { code: 'inference_failed' });
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
    const safe = ['unsupported_auth','identity_changed','sdk_version','node_version','unsupported_backend','model_changed','settings_changed','unsupported_transport','profile_authority','helper_authority','extension_authority','configuration_changed','provider_configuration','missing_credentials','no_chat_models','unsafe_model_configuration'];
    emit('error', { code: error instanceof HostError && safe.includes(error.code) ? error.code : 'host_refused' });
    process.exitCode = 1;
  });
}
