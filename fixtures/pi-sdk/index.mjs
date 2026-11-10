// Hermetic fixture of the pinned SDK signatures/events, never an emulation of stock Pi CLI.
import fs from 'node:fs';
const statePath = AF_FIXTURE_STATE;
const state = () => JSON.parse(fs.readFileSync(statePath, 'utf8'));
const token = (account,nonce='original') => 'e30.' + Buffer.from(JSON.stringify({'https://api.openai.com/auth':{chatgpt_account_id:account},nonce})).toString('base64url') + '.synthetic';
const backend = () => state().backend ?? 'openai-codex';
const model = () => state().model ? {id:state().model,api:state().api ?? 'openai-completions',provider:backend(),baseUrl:state().endpoint ?? 'https://custom.invalid/v1',reasoning:state().reasoning ?? true,maxTokens:32768} : backend() === 'kimi-coding'
  ? { id: 'k3', api: 'openai-completions', provider: backend(), baseUrl: state().endpoint ?? 'https://api.kimi.com/coding/v1', reasoning: true, maxTokens: 32768 }
  : { id: 'o3', api: 'openai-codex-responses', provider: backend(), baseUrl: state().endpoint ?? 'https://chatgpt.com/backend-api', reasoning: true, maxTokens: 32768 };
export const createExtensionRuntime = () => ({});
export class ModelRuntime {
  static async create(options) {
    if (options.allowModelNetwork || options.refreshOnCreate) throw Error('ambient runtime');
    const configured = options.modelsPath ? JSON.parse(fs.readFileSync(options.modelsPath,'utf8')).providers[backend()] : undefined;
    let catalog = configured?.models?.map(m => ({...model(),...m,provider:backend(),api:m.api ?? configured.api ?? model().api,baseUrl:m.baseUrl ?? configured.baseUrl ?? model().baseUrl})) ?? [model()];
    return {
      registerProvider: (id,config) => { if(id!==backend())throw Error("provider scope"); catalog=config.models.map(m=>({...model(),...m,provider:id,api:m.api ?? config.api,baseUrl:m.baseUrl ?? config.baseUrl})); },
      getProviders: () => [{id:backend(),auth:{oauth:{toAuth:async()=>({apiKey:'synthetic'})}}}],
      getAuth: async (selected,config) => { const id=typeof selected==='string'?selected:selected.provider; return (id === backend() && ((await options.credentials.read(id)) || configured?.apiKey || Object.keys(config?.env ?? {}).length)) ? {auth:{apiKey:'synthetic'}} : undefined; },
      getError: () => undefined,
      refresh: async () => { const cached=await options.modelsStore?.read(backend()); if(cached?.models)catalog=[...catalog,...cached.models]; return {errors:new Map()}; },
      getModels: () => catalog.filter(m=>!m.type || m.type==='chat'), getModel: (provider,id) => provider === backend() ? catalog.find(m=>m.id===id) : undefined,
      async streamSimple(selected,_context,options) {
        fs.appendFileSync(statePath+'.native',JSON.stringify({maxTokens:options.maxTokens})+'\n');
        if (options.maxRetries !== 0) throw Error('ambient auth/retry');
        await options.credentials?.read?.(backend());
        if (state().providerEvents) {
          for (const event of state().providerEvents) await options.onProviderStreamEvent(event, selected);
          return;
        }
        if (backend() !== 'openai-codex') {
          const usage = state().usage ?? {prompt_tokens:12,completion_tokens:3,cached_tokens:2,completion_tokens_details:{reasoning_tokens:1}};
          for (let n=0;n<(state().usageEvents ?? 2);n++) await options.onProviderStreamEvent(n%2 ? {choices:[{usage}]} : {choices:[],usage}, selected);
          return;
        }
        await options.onProviderStreamEvent({type:'response.completed',response:{usage:{input_tokens:3,output_tokens:2,input_tokens_details:{cached_tokens:0},output_tokens_details:{reasoning_tokens:1}}}}, selected);
      },
      credentials: options.credentials,
    };
  }
}
export class SettingsManager {
  static inMemory(settings,options) { return {settings,options}; }
}
export class SessionManager {
  static inMemory(cwd) { return {cwd}; }
}
export async function createAgentSession(options) {
  const settings=options.settingsManager.settings;
  if (settings.retry.enabled || settings.retry.provider.maxRetries || settings.compaction.enabled || settings.cacheWarming !== 'off') throw Error('unsafe settings');
  if (options.resourceLoader.getExtensions().extensions.length || options.resourceLoader.getAgentsFiles().agentsFiles.length || options.noTools !== 'all') throw Error('ambient resources');
  if (options.tools.some(n=>!['read','ls','find','grep','write','edit'].includes(n))) throw Error('unsafe tools');
  const listeners=[];
  const emit=(event)=>listeners.forEach(fn=>fn(event));
  const session={
    model:options.model,thinkingLevel:options.thinkingLevel,
    setAutoRetryEnabled(enabled) { if (enabled) throw Error('retry'); },
    setAutoCompactionEnabled(enabled) { if (enabled) throw Error('compaction'); },
    getActiveToolNames:()=>options.tools,subscribe:(fn)=>listeners.push(fn),dispose:()=>{},
    async prompt(text, promptOptions) {
      if (promptOptions.expandPromptTemplates !== false) throw Error('template expansion');
      const mode=state().mode;
      const probe=text.startsWith('This is a Provider admission check') || text.startsWith('Automated readiness check');
      fs.appendFileSync(statePath+'.calls',(probe?'admission':'worker')+'\n');
      await options.modelRuntime.credentials.read(backend());
      let answer='OK';
      if (!probe) {
        const request = ['document','retry','rotate-after-admission'].includes(mode) ? JSON.parse(text) : undefined;
        if (mode==='document' || mode==='rotate-after-admission') {
          const sources=request.inputs.sources[0].payload.sources;
          answer=JSON.stringify({schema:'af.worker-reply/1',outputs:{draft:[{schema:'af.document-draft/3',title:'Release notes',sections:[{heading:'Summary',body:Object.keys(sources).sort().map(k=>sources[k].text).join('\n\n')}],citations:Object.keys(sources).sort().map(name=>({kind:'source',name}))}]}});
        } else if (mode==='retry' && !request.feedback.length) answer='not a typed reply';
        else if (mode==='retry') {
          const sources=request.inputs.sources[0].payload.sources;
          answer=JSON.stringify({schema:'af.worker-reply/1',outputs:{draft:[{schema:'af.document-draft/3',title:'Release notes',sections:[{heading:'Summary',body:Object.keys(sources).sort().map(k=>sources[k].text).join('\n\n')}],citations:Object.keys(sources).sort().map(name=>({kind:'source',name}))}]}});
        } else answer=JSON.stringify({findings:[],benchmark_demands:[],dispositions:[]});
      }
      if (mode === 'kimi-tool') {
        await options.customTools.find(t=>t.name==='write').execute('tool',{path:'kimi-edited',content:'confined Kimi edit'});
      }
      const message={role:'assistant',provider:backend(),model:options.model.id,stopReason:state().stopReason ?? 'stop',content:[{type:'text',text:answer}]};
      for (let i=0;i<(state().inferences ?? 1);i++) {
        emit({type:'message_start',message});
        try { await options.modelRuntime.streamSimple(options.model,{},{}); }
        catch (error) { if (!state().continueAfterNativeError) throw error; }
        emit({type:'message_end',message:state().wrongMessageModel ? {...message,model:'wrong-model'} : message});
        if (state().countRead) await options.customTools.find(t=>t.name==='read').execute('id',{path:'SYNTHETIC_SECRET_PATH'});
      }
      if (state().lateError !== undefined) emit({type:'message_end',message:{...message,
        stopReason:state().lateStopReason ?? 'error',errorMessage:state().lateError}});
      if (state().finalAfterError) emit({type:'message_end',message:{...message,stopReason:'stop'}});
      emit({type:'agent_end'});
      if (!state().unsettled) emit({type:'agent_settled',aborted:state().aborted ?? false});
      if (state().promptError !== undefined) throw Error(state().promptError);
      if (probe && mode==='replace-auth-after-admission') {
        const data=JSON.parse(fs.readFileSync(state().auth,'utf8'));data[backend()]={type:'api_key',key:'PRIVATE_HELPER'};
        fs.writeFileSync(state().auth,JSON.stringify(data));
      }
      if (probe && mode==='replace-endpoint-after-admission') {
        const next=state(); next.endpoint='https://unexpected.invalid'; fs.writeFileSync(statePath,JSON.stringify(next));
      }
      if (probe && mode==='rotate-after-admission') {
        const data=JSON.parse(fs.readFileSync(state().auth,'utf8')); data['openai-codex'].access=token('synthetic-account','rotation');
        data['openai-codex'].refresh='ROTATED_REFRESH';fs.writeFileSync(state().auth,JSON.stringify(data));
      }
      if (probe && mode==='replace-after-admission') {
        const data=JSON.parse(fs.readFileSync(state().auth,'utf8'));data['openai-codex'].accountId='replacement-account'; data['openai-codex'].access=token('replacement-account');
        fs.writeFileSync(state().auth,JSON.stringify(data));
      }
    },
  };
  return {session,extensionsResult:options.resourceLoader.getExtensions()};
}
