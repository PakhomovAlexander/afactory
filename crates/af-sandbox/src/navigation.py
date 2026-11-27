# Fixed broker program, never a caller-supplied command. Python isolated mode, no imports from source.
import io
import json
import os
import stat
import sys


def fail():
    raise ValueError('navigation denied')


def pairs(items):
    result = {}
    for key, value in items:
        if key in result:
            fail()
        result[key] = value
    return result


def path_ok(path):
    return (isinstance(path, str) and 0 < len(path) <= 240 and path.isascii()
            and all(c and c.lower() not in ('.', '..', '.git', '.hg', '.svn')
                    and all(x.isalnum() or x in '._-' for x in c)
                    for c in path.split('/')))


def read(path):
    if not path_ok(path):
        fail()
    current = '/source'
    for component in path.split('/'):
        current += '/' + component
        info = os.lstat(current)
        if stat.S_ISLNK(info.st_mode):
            fail()
    if not stat.S_ISREG(info.st_mode) or info.st_size > 8 * 1024 * 1024:
        fail()
    fd = os.open(current, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        data = stream.read(8 * 1024 * 1024 + 1)
    if len(data) > 8 * 1024 * 1024:
        fail()
    return data


def run():
    raw = sys.stdin.buffer.read(4097)
    if len(raw) > 4096:
        fail()
    request = json.loads(raw, object_pairs_hook=pairs)
    tool = request.get('tool')
    if tool == 'af_read_range':
        if set(request) != {'tool', 'path', 'offset', 'length'}:
            fail()
        offset, length = request['offset'], request['length']
        if (type(offset) is not int or not 0 <= offset <= 4294967295
                or type(length) is not int or not 1 <= length <= 65536):
            fail()
        data = read(request['path'])
        if offset > len(data):
            fail()
        try:
            data.decode('utf-8')
        except UnicodeDecodeError:
            result = {'tool': tool, 'text': None}
        else:
            result = {'tool': tool, 'text': data[offset:offset + length].decode('utf-8')}
    elif tool in ('af_list_paths', 'af_search'):
        if set(request) != ({'tool', 'offset'} if tool == 'af_list_paths' else {'tool', 'query', 'offset'}):
            fail()
        query = request.get('query')
        if tool == 'af_search' and (not isinstance(query, str) or not query or not query.isascii()
                or len(query.encode()) > 256 or any(c in query for c in '\n\r\0')):
            fail()
        paths = []
        for root, dirs, files in os.walk('/source', followlinks=False):
            for name in dirs + files:
                full = os.path.join(root, name)
                relative = os.path.relpath(full, '/source')
                if not path_ok(relative) or os.path.islink(full):
                    fail()
                if name in files:
                    paths.append(relative)
                    if len(paths) > 16384:
                        fail()
        paths.sort()
        if tool == 'af_list_paths':
            # Validate regular-file boundaries for listings too.
            for path in paths:
                read(path)
            offset = request['offset']
            if type(offset) is not int or not 0 <= offset <= len(paths):
                fail()
            end = offset + 256
            result = {'tool': tool, 'paths': paths[offset:end],
                      'next_offset': end if end < len(paths) else None}
        else:
            offset = request['offset']
            if type(offset) is not int or not 0 <= offset <= 4294967295:
                fail()
            hits, non_text_paths = [], []
            matched, next_offset, retained_bytes = 0, None, 0
            for path in paths:
                data = read(path)
                try:
                    data.decode('utf-8')
                except UnicodeDecodeError:
                    non_text_paths.append(path)
                    retained_bytes += len(json.dumps(path, ensure_ascii=True))
                    if retained_bytes > 262144:
                        fail()
                    continue
                # LF-only iteration retains one line, even for millions of short lines.
                for number, raw_line in enumerate(io.BytesIO(data), 1):
                    line = raw_line.removesuffix(b'\n').decode('utf-8')
                    if query in line:
                        matched += 1
                        if matched <= offset:
                            continue
                        if len(hits) == 128:
                            next_offset = offset + 128
                            break
                        hit = {'path': path, 'line': number, 'text': line}
                        retained_bytes += len(json.dumps(hit, ensure_ascii=True))
                        if retained_bytes > 262144:
                            fail()
                        hits.append(hit)
                if next_offset is not None:
                    break
            if offset > matched:
                fail()
            result = {'tool': tool, 'hits': hits, 'next_offset': next_offset,
                      'non_text_paths': non_text_paths}

    else:
        fail()
    encoded = json.dumps(result, ensure_ascii=True, sort_keys=True, separators=(',', ':')).encode()
    if len(encoded) > 262144:
        fail()
    sys.stdout.buffer.write(encoded)


try:
    run()
except Exception:
    sys.stderr.write('navigation denied\n')
    sys.exit(1)
