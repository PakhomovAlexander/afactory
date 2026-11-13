"""Bounded, complete release and Actions metadata observations; never secret values."""
import json
import re
from urllib.parse import parse_qs, urlsplit


PAGE_SIZE = 100
MAX_PAGES = 10


def pages(api, endpoint, key, identity='id'):
    rows, seen, total = [], set(), None
    for page in range(1, MAX_PAGES + 1):
        data = api(endpoint + ('&' if '?' in endpoint else '?') +
                   'per_page=100&page=' + str(page))
        if not isinstance(data, dict):
            raise ValueError('incomplete_observation: invalid API collection')
        batch, count = data.get(key), data.get('total_count')
        if (type(count) is not int or count < 0 or (total is not None and total != count)
                or not isinstance(batch, list) or len(batch) > PAGE_SIZE):
            raise ValueError('incomplete_observation: missing/changing total or invalid page')
        total = count
        for row in batch:
            value = row.get(identity) if isinstance(row, dict) else None
            valid = (isinstance(value, str) and bool(value)) if identity == 'name' else type(value) is int
            if not valid or value in seen:
                raise ValueError('incomplete_observation: ambiguous API collection identities')
            seen.add(value)
        rows.extend(batch)
        if len(rows) > total or (len(batch) < PAGE_SIZE and len(rows) != total):
            raise ValueError('incomplete_observation: partial API collection')
        if len(rows) == total:
            return rows
    raise ValueError('observation_budget: API collection exceeds 1000 entries')


def releases(read):
    """Require two matching, complete traversals; this is not an atomic snapshot."""
    first = _release_traversal(read)
    if _release_traversal(read) != first:
        raise ValueError('incomplete_observation: release collection changed; retry observation')
    return first[0]


def _release_traversal(read):
    rows, ids, tags, pagination = [], set(), set(), []
    expected_next = False
    advertised_last = None
    for page in range(1, MAX_PAGES + 1):
        raw = read('releases?per_page=100&page=' + str(page))
        parts = re.split(r'\r?\n\r?\n', raw, maxsplit=1)
        if len(parts) != 2 or not re.match(r'HTTP/\S+ 200(?:\s|$)', parts[0]):
            raise ValueError('incomplete_observation: missing successful release response headers')
        headers, body = parts
        batch = json.loads(body)
        if (not isinstance(batch, list) or len(batch) > PAGE_SIZE
                or (expected_next and not batch)):
            raise ValueError('incomplete_observation: invalid release page')
        links = [line.split(':', 1)[1] for line in headers.splitlines()
                 if line.lower().startswith('link:')]
        parsed_links, relations = [], {}
        for member in ','.join(links).split(',') if links else []:
            match = re.fullmatch(r'\s*<([^>]+)>;\s*rel="(next|prev|first|last)"\s*', member)
            if not match:
                raise ValueError('incomplete_observation: invalid release pagination metadata')
            url, rel = match.groups()
            query = parse_qs(urlsplit(url).query, keep_blank_values=True)
            numbers = query.get('page', [])
            if (rel in relations or len(numbers) != 1
                    or not re.fullmatch(r'[1-9][0-9]*', numbers[0])
                    or query.get('per_page') != ['100']):
                raise ValueError('incomplete_observation: invalid/duplicate release pagination relation')
            relations[rel] = int(numbers[0])
            parsed_links.append((url, rel))
        pagination.append(parsed_links)
        if ('prev' in relations and relations['prev'] != page - 1
                or 'first' in relations and relations['first'] != 1
                or 'next' in relations and relations['next'] != page + 1):
            raise ValueError('incomplete_observation: contradictory release pagination relation')
        if 'last' in relations:
            if advertised_last is not None and advertised_last != relations['last']:
                raise ValueError('incomplete_observation: changing release last page')
            advertised_last = relations['last']
        if advertised_last is not None:
            if (page > advertised_last
                    or ('next' in relations and page >= advertised_last)
                    or ('next' not in relations and page != advertised_last)):
                raise ValueError('incomplete_observation: contradictory release end; '
                                 'investigate pagination and retry')
        for row in batch:
            if (not isinstance(row, dict) or type(row.get('id')) is not int
                    or not isinstance(row.get('tag_name'), str) or not row['tag_name']
                    or row['id'] in ids or row['tag_name'] in tags):
                raise ValueError('incomplete_observation: conflicting release identities')
            ids.add(row['id'])
            tags.add(row['tag_name'])
        rows.extend(batch)
        if 'next' not in relations:
            # A prev link alone cannot prove that a full later page is terminal.
            # GitHub may omit last on the terminal page; retain earlier end evidence.
            if page > 1 and 'prev' not in relations:
                raise ValueError('incomplete_observation: missing release pagination metadata; '
                                 'investigate pagination and retry')
            if len(batch) == PAGE_SIZE and advertised_last != page:
                raise ValueError('incomplete_observation: full release page lacks next metadata '
                                 'and authoritative last page; investigate pagination and retry')
            return rows, pagination
        if len(batch) != PAGE_SIZE:
            raise ValueError('incomplete_observation: partial or missing release page')
        expected_next = True
    raise ValueError('observation_budget: release termination unproven within 10 pages; '
                     'retry after investigating release pagination and the observation bound')
