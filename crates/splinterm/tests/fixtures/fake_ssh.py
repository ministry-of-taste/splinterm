#!/usr/bin/python3
import json, os, struct, sys

base = os.path.dirname(os.path.realpath(sys.argv[0]))
record = os.path.join(base, 'argv.json')
with open(record, 'w', encoding='utf-8') as handle:
    json.dump(sys.argv[1:], handle)
count = os.path.join(base, 'count')
requests = os.path.join(base, 'requests.jsonl')
events = os.path.join(base, 'events.jsonl')
mode = os.environ.get('SPLINTERM_FAKE_SSH_MODE', 'read-only')
LAIR_ID = '11111111-1111-4111-8111-111111111111'
DOJO_ID = '22222222-2222-4222-8222-222222222222'
MISMATCHED_SPLINT_ID = 'ffffffff-ffff-4fff-8fff-ffffffffffff'
CAPPED_COLUMNS = 120
CAPPED_ROWS = 64
try:
    value = int(open(count, encoding='utf-8').read())
except Exception:
    value = 0
instance = value + 1
with open(count, 'w', encoding='utf-8') as handle:
    handle.write(str(instance))
with open(os.path.join(base, 'pid-' + str(instance)), 'w', encoding='utf-8') as handle:
    handle.write(str(os.getpid()))

stdin = sys.stdin.buffer
stdout = sys.stdout.buffer
MAGIC = b'SPGR'
VERSION = 1

def exact(count):
    value = b''
    while len(value) < count:
        chunk = stdin.read(count - len(value))
        if not chunk:
            return None
        value += chunk
    return value

def outer_read():
    header = exact(16)
    if header is None:
        return None
    magic, version, kind, flags, channel, length = struct.unpack('>4sHBBII', header)
    if magic != MAGIC or version != VERSION or flags != 0:
        raise SystemExit(3)
    payload = exact(length)
    if payload is None:
        raise SystemExit(4)
    return kind, channel, payload

def outer_write(kind, channel=0, payload=b''):
    stdout.write(struct.pack('>4sHBBII', MAGIC, VERSION, kind, 0, channel, len(payload)))
    stdout.write(payload)
    stdout.flush()

def private_write(channel, value):
    body = json.dumps(value, separators=(',', ':')).encode()
    outer_write(6, channel, struct.pack('>I', len(body)) + body)

def capped_lairs():
    return [{
        'id': LAIR_ID,
        'name': 'capped-fixture',
        'retention': 'disposable',
        'dojos': [{
            'id': DOJO_ID,
            'name': 'Dojo 1',
            'default_focus': MISMATCHED_SPLINT_ID,
            'root': {'Leaf': {
                'id': MISMATCHED_SPLINT_ID,
                'title': 'capped fixture',
                'cwd': '/tmp',
                'command': [],
                'launch': {
                    'shell': None,
                    'login_shell': False,
                    'scrollback_lines': 10000,
                    'columns': CAPPED_COLUMNS,
                    'rows': CAPPED_ROWS,
                    'relaunch_on_restore': False
                },
                'last_incarnation': 1,
                'state': 'Running'
            }}
        }]
    }]

def capped_snapshot(splint_id, incarnation):
    edge = 'CAP_EDGE'
    visible_rows = []
    for row in range(CAPPED_ROWS):
        cells = []
        if row == 1:
            cells = [{'content': value} for value in 'CAPPED 120x64 FIXTURE']
        elif row == 10:
            cells = ([{}] * (CAPPED_COLUMNS - len(edge))
                     + [{'content': value} for value in edge])
        visible_rows.append({'row_id': row + 1, 'cells': cells})
    return {
        'splint_id': splint_id,
        'incarnation': incarnation,
        'revision': 11,
        'columns': CAPPED_COLUMNS,
        'rows': CAPPED_ROWS,
        'cursor_column': CAPPED_COLUMNS - 1,
        'cursor_row': 10,
        'cursor_deferred_wrap': False,
        'active_screen': 'normal',
        'input_modes': {
            'application_cursor': False,
            'application_keypad': False,
            'focus_reporting': False,
            'bracketed_paste': False,
            'cursor_visible': True,
            'cursor_blink': False,
            'mouse_tracking': 'none',
            'sgr_mouse': False
        },
        'palette': [0] * 256,
        'default_colors': [0, 0, 0],
        'title': 'capped fixture',
        'visible_rows': visible_rows,
        'history_generation': 3,
        'oldest_available_scrollback_row_id': None,
        'newest_available_scrollback_row_id': None,
        'scrollback_rows': [],
        'available_scrollback_rows': 0,
        'omitted_oldest_scrollback_rows': 0,
        'exited_code': None,
        'exited_signal': None
    }

hello = outer_read()
if hello != (1, 0, b''):
    raise SystemExit(5)
outer_write(2)
buffers = {}
handshaken = set()
access_grants = {}
closed_once = False
while True:
    frame = outer_read()
    if frame is None:
        break
    kind, channel, payload = frame
    if kind == 3:
        buffers[channel] = b''
        if mode != 'stall-channel-open':
            outer_write(4, channel)
    elif kind == 6:
        buffers[channel] = buffers.get(channel, b'') + payload
        while len(buffers[channel]) >= 4:
            length = struct.unpack('>I', buffers[channel][:4])[0]
            if len(buffers[channel]) < 4 + length:
                break
            body = buffers[channel][4:4+length]
            buffers[channel] = buffers[channel][4+length:]
            message = json.loads(body)
            if channel not in handshaken:
                if message.get('type') != 'hello' or message.get('role') != 'remote_interactive':
                    raise SystemExit(6)
                if mode == 'close-first-private-hello' and not closed_once:
                    closed_once = True
                    buffers[channel] = b''
                    outer_write(8, channel)
                    break
                handshaken.add(channel)
                if mode == 'stall-private-hello':
                    continue
                private_write(channel, {
                    'type': 'hello',
                    'version': 45,
                    'limits': {
                        'maximum_frame_bytes': 16777216,
                        'maximum_input_bytes': 65536,
                        'maximum_columns': CAPPED_COLUMNS if mode == 'graphical-capped' else 480,
                        'maximum_rows': CAPPED_ROWS if mode == 'graphical-capped' else 128,
                        'maximum_outstanding_requests': 1,
                        'maximum_subscriptions': 4,
                        'maximum_snapshot_scrollback_rows': 16,
                        'image': {
                            'metadata_version': 1,
                            'binary_chunks': False,
                            'sealed_memfd': False,
                            'maximum_content_bytes': 1,
                            'maximum_bytes_per_splint': 1,
                            'maximum_bytes_per_daemon': 1,
                            'maximum_contents_per_splint': 1,
                            'maximum_placements_per_splint': 1,
                            'maximum_dimension': 1,
                            'maximum_pixels': 1,
                            'maximum_chunk_bytes': 1,
                            'maximum_chunk_window': 1,
                            'maximum_transfers_per_splint': 1,
                            'maximum_transfers_per_daemon': 1
                        }
                    },
                    'development_terminal_access': False,
                    'daemon_generation': 1
                })
            else:
                request_id = message['request_id']
                request = message['request']
                with open(requests, 'a', encoding='utf-8') as handle:
                    handle.write(json.dumps(request, separators=(',', ':')) + '\n')
                if mode == 'close-first' and request['type'] == 'ping' and not closed_once:
                    closed_once = True
                    buffers[channel] = b''
                    handshaken.discard(channel)
                    outer_write(8, channel)
                    break
                if mode == 'denied' and request['type'] == 'list_lairs':
                    private_write(channel, {
                        'type': 'error',
                        'request_id': request_id,
                        'error': {'code': 'unauthorized', 'message': 'fixture daemon denied topology read'}
                    })
                    continue
                if mode == 'read-only-pane' and request['type'] == 'request_access' and any(
                    scope in ('input', 'resize') for scope in request['scopes']
                ):
                    private_write(channel, {
                        'type': 'error',
                        'request_id': request_id,
                        'error': {'code': 'unauthorized', 'message': 'fixture denied interactive access'}
                    })
                    continue
                if mode in ('denied-interactive', 'read-only-pane') and request['type'] in ('acquire_control', 'input', 'resize'):
                    private_write(channel, {
                        'type': 'error',
                        'request_id': request_id,
                        'error': {'code': 'unauthorized', 'message': 'fixture denied interactive control'}
                    })
                    continue
                if request['type'] == 'ping':
                    result = {'type': 'pong'}
                elif request['type'] == 'list_lairs':
                    result = {
                        'type': 'lairs',
                        'lairs': capped_lairs() if mode == 'graphical-capped' else [],
                        'topology_revision': 1
                    }
                elif request['type'] == 'inspect_splint' and mode == 'graphical-capped':
                    result = {
                        'type': 'splint',
                        'lair_id': LAIR_ID,
                        'dojo_id': DOJO_ID,
                        'title': 'capped fixture',
                        'topology_revision': 1,
                        'runtime': {
                            'splint_id': MISMATCHED_SPLINT_ID,
                            'live_incarnation': 1,
                            'last_incarnation': 1,
                            'restorable': False,
                            'lifecycle': 'running',
                            'exit_status': None
                        }
                    }
                elif request['type'] == 'request_access':
                    grant = {
                        'grant_id': 41,
                        'splint_id': request['splint_id'],
                        'incarnation': request['incarnation'],
                        'scopes': request['scopes'],
                        'requester': 'fake-ssh',
                        'expires_at_unix_seconds': 4102444800
                    }
                    access_grants[(request['splint_id'], request['incarnation'])] = grant
                    result = {
                        'type': 'access_granted',
                        'lair_id': LAIR_ID,
                        'dojo_id': DOJO_ID,
                        'authorization_revision': 1,
                        'grant': grant
                    }
                elif request['type'] == 'authorization_status':
                    incarnation = request['incarnation'] or 1
                    grant = access_grants.get((request['splint_id'], incarnation))
                    result = {
                        'type': 'authorization_status',
                        'lair_id': LAIR_ID,
                        'dojo_id': DOJO_ID,
                        'incarnation': incarnation,
                        'topology_revision': 1,
                        'policy_generation': 1,
                        'grants': [] if grant is None else [grant],
                        'lair_grants': [],
                        'persistent': [],
                        'development_bypass': False
                    }
                elif request['type'] == 'attach':
                    splint_id = request['splint_id']
                    incarnation = request['incarnation'] or 1
                    snapshot = (capped_snapshot(splint_id, incarnation)
                                if mode == 'graphical-capped' else {
                        'splint_id': splint_id,
                        'incarnation': incarnation,
                        'revision': 11,
                        'columns': 1,
                        'rows': 1,
                        'cursor_column': 0,
                        'cursor_row': 0,
                        'cursor_deferred_wrap': False,
                        'active_screen': 'normal',
                        'input_modes': {
                            'application_cursor': False,
                            'application_keypad': False,
                            'focus_reporting': False,
                            'bracketed_paste': False,
                            'cursor_visible': True,
                            'cursor_blink': False,
                            'mouse_tracking': 'none',
                            'sgr_mouse': False
                        },
                        'palette': [0] * 256,
                        'default_colors': [0, 0, 0],
                        'title': 'read-only fixture',
                        'visible_rows': [{'row_id': 1, 'cells': []}],
                        'history_generation': 3,
                        'oldest_available_scrollback_row_id': None,
                        'newest_available_scrollback_row_id': None,
                        'scrollback_rows': [],
                        'available_scrollback_rows': 0,
                        'omitted_oldest_scrollback_rows': 0,
                        'exited_code': None,
                        'exited_signal': None
                    })
                    result = {
                        'type': 'attached',
                        'subscription_id': 73,
                        'provenance': {
                            'lair_id': LAIR_ID,
                            'dojo_id': DOJO_ID,
                            'splint_id': splint_id,
                            'incarnation': incarnation,
                            'topology_revision': 1,
                            'terminal_revision': 11,
                            'history_generation': 3,
                            'title': 'read-only fixture'
                        },
                        'snapshot': snapshot
                    }
                elif request['type'] == 'acquire_control':
                    result = {
                        'type': 'control_granted',
                        'controller_id': 91,
                        'lair_id': LAIR_ID,
                        'dojo_id': DOJO_ID
                    }
                elif request['type'] == 'subscribe_control':
                    result = {
                        'type': 'control_subscribed',
                        'subscription_id': 77,
                        'status': {
                            'splint_id': request['splint_id'],
                            'incarnation': request['incarnation'],
                            'controlled': True,
                            'locally_owned': True
                        }
                    }
                elif request['type'] in ('input', 'resize'):
                    result = {
                        'type': 'terminal_action_acknowledged',
                        'lair_id': LAIR_ID,
                        'dojo_id': DOJO_ID,
                        'splint_id': MISMATCHED_SPLINT_ID if mode == 'mismatched-identity' else request['splint_id'],
                        'incarnation': request['incarnation'],
                        'terminal_revision': 12,
                        'history_generation': 3
                    }
                elif request['type'] in ('release_control', 'detach'):
                    result = {'type': 'acknowledged'}
                elif request['type'] == 'subscribe_consent_offers':
                    result = {'type': 'consent_offers_subscribed', 'subscription_id': 5}
                elif request['type'] == 'answer_consent_offer':
                    result = {'type': 'consent_offer_answered'}
                else:
                    raise SystemExit(7)
                private_write(channel, {
                    'type': 'response',
                    'request_id': request_id,
                    'result': result
                })
                # A consent responder's first offer, as the test wrote it.
                if request['type'] == 'subscribe_consent_offers':
                    with open(os.path.join(base, 'consent-offer.json'), encoding='utf-8') as handle:
                        private_write(channel, json.load(handle))
    elif kind == 7:
        pass
    elif kind == 8:
        with open(events, 'a', encoding='utf-8') as handle:
            handle.write('close:' + str(channel) + '\n')
        buffers.pop(channel, None)
        handshaken.discard(channel)
        outer_write(8, channel)
    else:
        raise SystemExit(8)
