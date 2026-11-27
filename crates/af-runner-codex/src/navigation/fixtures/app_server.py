#!/usr/bin/python3
"""Deterministic stdio app-server peer. Dummy auth, loopback fixture only."""
import http.client
import json
import os
import sys
from urllib.parse import urlsplit

if '--version' in sys.argv:
    print('codex-cli 0.167.2')
    sys.exit(0)

config = {}
for i, arg in enumerate(sys.argv):
    if arg == '-c':
        k, v = sys.argv[i + 1].split('=', 1)
        try:
            config[k] = json.loads(v)
        except ValueError:
            config[k] = v


def emit(value):
    print(json.dumps(value), flush=True)


def inference(tools):
    url = urlsplit(config['model_providers.af_navigation.base_url'])
    assert url.hostname == '127.0.0.1'
    c = http.client.HTTPConnection(url.hostname, url.port, timeout=30)
    body = {'model': 'gpt-6.1-sol', 'stream': True, 'reasoning': {'effort': 'medium'}, 'input': [{'type': 'additional_tools', 'tools': [{'type': 'namespace', 'name': 'functions', 'tools': tools}]}]}
    c.request('POST', '/v1/responses', json.dumps(body), {
        'Authorization': 'Bearer dummy-native-access',
        'ChatGPT-Account-ID': 'fixture-account',
        'X-AF-Gateway': os.environ[config['model_providers.af_navigation.env_http_headers.x-af-gateway']],
    })
    response = c.getresponse()
    data = response.read()
    c.close()
    if response.status != 200:
        emit({'method': 'error', 'params': {}})
        return None
    events = [json.loads(line[5:]) for line in data.decode().splitlines()
              if line.startswith('data:') and line[5:].strip() != '[DONE]']
    return events


tools = []
for line in sys.stdin:
    message = json.loads(line)
    method = message.get('method')
    if method == 'initialize':
        emit({'id': 1, 'result': {}})
    elif method == 'thread/start':
        params = message['params']
        assert params['environments'] == []
        assert params['ephemeral'] and not params['allowProviderModelFallback']
        assert params['approvalPolicy'] == 'never'
        tools = [{**tool, 'parameters': tool['inputSchema']} for tool in params['dynamicTools']]
        for tool in tools:
            del tool['inputSchema']
            for prop in tool['parameters'].get('properties', {}).values():
                prop.pop('minimum', None)
                prop.pop('maximum', None)
                prop.pop('minLength', None)
                prop.pop('maxLength', None)
                prop.pop('pattern', None)
        tools.append({'type': 'function', 'name': 'request_user_input', 'parameters': {}})
        emit({'id': 2, 'result': {'model': params['model'], 'modelProvider': params['modelProvider'],
              'approvalPolicy': 'never', 'sandbox': {'type': 'readOnly'}, 'thread': {'id': 'thread-fixture'}}})
    elif method == 'turn/start':
        emit({'id': 3, 'result': {'turn': {'id': 'turn-fixture'}}})
        events = inference(tools)
        if events is None:
            break
        for event in events:
            if event['type'] == 'response.output_item.done' and event['item']['type'] == 'function_call':
                item = event['item']
                if item['name'] == 'request_user_input':
                    emit({'id': 4, 'method': 'item/tool/requestUserInput', 'params': {}})
                else:
                    emit({'id': 4, 'method': 'item/tool/call', 'params': {
                        'callId': item['call_id'], 'threadId': 'thread-fixture', 'turnId': 'turn-fixture',
                        'tool': item['name'], 'arguments': json.loads(item['arguments']), 'namespace': None}})
    elif message.get('id') == 4:
        if 'error' in message:
            break
        assert message['result']['success']
        events = inference(tools)
        if events is None:
            break
        emit({'method': 'item/completed', 'params': {'item': {'type': 'agentMessage', 'text': 'src/lib.rs'}}})
        emit({'method': 'turn/completed', 'params': {'threadId': 'thread-fixture', 'turn': {'id': 'turn-fixture', 'status': 'completed'}}})
