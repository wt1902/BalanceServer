#!/usr/bin/env python3
"""Black-box Go/Rust compatibility and bilateral SQLite takeover checks (stdlib only)."""
import argparse
import base64
import concurrent.futures
import contextlib
import http.client
import hashlib
import hmac
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
SECRET = 'compatibility-test-secret-not-for-deployment-0123456789'
PASSWORD = 'compatibility password'


class Server:
    def __init__(self, binary, database, **extra):
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            self.port = sock.getsockname()[1]
        env = os.environ.copy()
        env.update(BALANCE_ADDR=f'127.0.0.1:{self.port}', BALANCE_DB_PATH=str(database),
                   BALANCE_JWT_SECRET=SECRET, BALANCE_ACCESS_TTL='15m',
                   BALANCE_REFRESH_TTL='720h', BALANCE_ALLOW_REGISTRATION='true',
                   BALANCE_CORS_ORIGINS='https://allowed.example')
        env.update(extra)
        self.log = tempfile.TemporaryFile()
        self.process = subprocess.Popen([str(binary)], env=env, cwd=ROOT,
                                        stdout=self.log, stderr=self.log)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                self.log.seek(0)
                raise AssertionError(f'{binary.name} exited: {self.log.read().decode()}')
            try:
                if self.request('GET', '/health')[0] == 200:
                    return
            except (OSError, http.client.HTTPException):
                pass
            time.sleep(.05)
        self.close()
        raise AssertionError(f'{binary.name} startup timeout')

    def close(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        self.log.close()

    def request(self, method, path, body=None, token=None, headers=None, raw=None, decode_json=True):
        hdr = dict(headers or {})
        if token:
            hdr['Authorization'] = f'Bearer {token}'
        if body is not None:
            raw = json.dumps(body).encode()
            hdr['Content-Type'] = 'application/json'
        connection = http.client.HTTPConnection('127.0.0.1', self.port, timeout=30)
        try:
            connection.request(method, path, body=raw, headers=hdr)
            response = connection.getresponse()
            data = response.read()
            values = response.getheaders()
            content_type = response.getheader('Content-Type', '')
            if decode_json and data and 'application/json' in content_type:
                data = json.loads(data)
            return response.status, values, data
        finally:
            connection.close()


@contextlib.contextmanager
def running(binary, database, **extra):
    server = Server(binary, database, **extra)
    try:
        yield server
    finally:
        server.close()


def expect(result, status):
    assert result[0] == status, f'expected HTTP {status}, got {result[0]}: {result[2]!r}'
    return result[2]


def credentials(email='User@Example.COM'):
    return {'email': email, 'password': PASSWORD}


def change(identifier, stamp='2025-01-01T00:00:00Z', **extra):
    result = dict(entity='transaction', id=identifier, updatedAt=stamp,
                  payload={'amount': 125.5, 'note': 'Тест €', 'nested': [None, True, 1]})
    result.update(extra)
    return result


def sync(server, token, changes=(), cursor=0, **extra):
    return expect(server.request('POST', '/v1/sync',
        dict(cursor=cursor, deviceId='compatibility', changes=list(changes), **extra), token), 200)


def scenario(binary, database):
    """Return deterministic observable results; assertions check generated sessions too."""
    observed = {}
    with running(binary, database) as server:
        for method, path, body in [
            ('GET', '/v1/me', None), ('GET', '/v1/missing', None),
            ('POST', '/v1/auth/register', {'email': 'bad', 'password': PASSWORD}),
            ('POST', '/v1/auth/register', {'email': 'a@b', 'password': 'short'}),
            ('POST', '/v1/auth/login', credentials()),
            ('POST', '/v1/auth/refresh', {}),
            ('POST', '/v1/auth/refresh', {'refreshToken': 'bad'}),
            ('POST', '/v1/auth/login', {'unknown': 1}),
        ]:
            result = server.request(method, path, body)
            observed[f'{method} {path} {body}'] = (result[0], result[2])
        for path in ['/','/app.js','/styles.css','/icon.svg','/manifest.webmanifest',
                     '/service-worker.js','/missing-file']:
            result = server.request('GET', path)
            head = {k.lower(): v for k, v in result[1]}
            keys = ['content-type','cache-control','x-content-type-options',
                    'x-frame-options','referrer-policy','permissions-policy','content-security-policy']
            observed[path] = (result[0], {k: head.get(k) for k in keys}, result[2])
            assert server.request('HEAD', path)[2] == b''
        for origin in ['https://allowed.example', 'https://denied.example']:
            result = server.request('OPTIONS', '/v1/sync', headers={'Origin': origin})
            observed[origin] = (result[0], {k.lower(): v for k, v in result[1]
                                           if k.lower().startswith('access-control') or k.lower() == 'vary'})
        session_result = server.request('POST', '/v1/auth/register', credentials(),
                                        headers={'X-Forwarded-Proto': 'https'})
        session = expect(session_result, 201)
        assert session['user']['email'] == 'user@example.com'
        cookies = [v for k, v in session_result[1] if k.lower() == 'set-cookie']
        assert len(cookies) == 2
        for cookie in cookies:
            assert all(part in cookie for part in ['HttpOnly','Secure','SameSite=Strict','Path=/'])
        cookie_header = '; '.join(c.split(';')[0] for c in cookies)
        assert expect(server.request('GET', '/v1/me', headers={'Cookie': cookie_header}), 200) == session['user']
        token = session['accessToken']
        assert expect(server.request('GET', '/v1/me', token=token), 200) == session['user']
        observed['invalid bearer'] = expect(server.request('GET', '/v1/me', token='bad'), 401)
        observed['duplicate'] = expect(server.request('POST', '/v1/auth/register', credentials()), 409)
        expect(server.request('POST', '/v1/auth/login', credentials()), 200)
        for payload in [{'cursor': -1}, {'extra': 1}, {'changes': [change('x', entity='unknown')]},
                        {'changes': [change('')]}, {'changes': [dict(entity='category', id='x')]},
                        {'changes': [change('x', payload=None)]}]:
            result = server.request('POST', '/v1/sync', payload, token)
            data = result[2]
            if isinstance(data, dict):
                data.pop('serverTime', None)
            observed[f'invalid sync {payload}'] = (result[0], data)
        observed['malformed'] = expect(server.request('POST', '/v1/sync', token=token, raw=b'{'), 400)
        first = sync(server, token, [change('one'), change('cat', entity='category'), change('budget', entity='budget')])
        first.pop('serverTime')
        observed['first'] = first
        repeated = sync(server, token, [change('one'), change('one', '2024-01-01T00:00:00Z')], cursor=first['cursor'])
        repeated.pop('serverTime')
        observed['idempotent'] = repeated
        assert repeated['changes'] == [] and repeated['cursor'] == first['cursor']
        deleted = sync(server, token, [change('one', '2025-01-02T00:00:00Z', deleted=True)], cursor=first['cursor'])
        deleted.pop('serverTime')
        observed['deleted'] = deleted
        assert deleted['changes'][0]['deleted'] and 'payload' not in deleted['changes'][0]
        pushed = sync(server, token, [change(f'page-{n}') for n in range(505)], cursor=deleted['cursor'], pull=False)
        assert pushed['cursor'] == deleted['cursor'] and pushed['changes'] == []
        page = sync(server, token, cursor=deleted['cursor'])
        assert len(page['changes']) == 500 and page['hasMore']
        tail = sync(server, token, cursor=page['cursor'])
        assert len(tail['changes']) == 5 and not tail['hasMore']
        observed['pagination'] = (page['cursor'], tail['cursor'])
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            results = list(pool.map(lambda n: sync(server, token, [change(f'parallel-{n}')], pull=False), range(16)))
        assert all(result['changes'] == [] for result in results)
        parallel = sync(server, token, cursor=tail['cursor'])
        assert len(parallel['changes']) == 16
        assert len({x['sequence'] for x in parallel['changes']}) == 16
        other = expect(server.request('POST', '/v1/auth/register', credentials('other@example.com')), 201)
        assert sync(server, other['accessToken'])['changes'] == []
        refreshed = expect(server.request('POST', '/v1/auth/refresh', {}, headers={'Cookie': cookie_header}), 200)
        assert refreshed['refreshToken'] != session['refreshToken']
        observed['refresh replay'] = expect(server.request('POST', '/v1/auth/refresh', {'refreshToken': session['refreshToken']}), 401)
        logged_out = server.request('POST', '/v1/auth/logout', {'refreshToken': refreshed['refreshToken']})
        expect(logged_out, 204)
        assert len([v for k,v in logged_out[1] if k.lower() == 'set-cookie' and 'Max-Age=0' in v]) == 2
        observed['logout refresh'] = expect(server.request('POST', '/v1/auth/refresh', {'refreshToken': refreshed['refreshToken']}), 401)
    # Fresh limiter state permits exactly 30 authentication attempts, including invalid JSON.
    with running(binary, database) as server:
        statuses = [server.request('POST', '/v1/auth/login', raw=b'{')[0] for _ in range(31)]
        assert statuses == [400] * 30 + [429], statuses
        result = server.request('POST', '/v1/auth/login', raw=b'{')
        assert {k.lower():v for k,v in result[1]}.get('retry-after') == '60'
        observed['rate limit'] = result[2]
    with running(binary, database, BALANCE_ALLOW_REGISTRATION='false', BALANCE_ACCESS_TTL='1s') as server:
        observed['registration disabled'] = expect(server.request('POST', '/v1/auth/register', credentials()), 403)
        session = expect(server.request('POST', '/v1/auth/login', credentials()), 200)
        time.sleep(1.15)
        observed['expired access'] = expect(server.request('GET', '/v1/me', token=session['accessToken']), 401)
    return observed


def takeover(first, second, database):
    with running(first, database) as server:
        session = expect(server.request('POST', '/v1/auth/register', credentials()), 201)
        original = sync(server, session['accessToken'], [change('persisted')])
    with running(second, database) as server:
        assert expect(server.request('GET', '/v1/me', token=session['accessToken']), 200) == session['user']
        assert expect(server.request('POST', '/v1/auth/login', credentials()), 200)['user'] == session['user']
        assert sync(server, session['accessToken'])['changes'] == original['changes']
        next_session = expect(server.request('POST', '/v1/auth/refresh', {'refreshToken': session['refreshToken']}), 200)
        updated = sync(server, next_session['accessToken'], [change('persisted','2025-01-02T00:00:00Z',deleted=True)], cursor=original['cursor'])
        assert len(updated['changes']) == 1 and updated['changes'][0]['version'] == 2
    with running(first, database) as server:
        assert expect(server.request('GET', '/v1/me', token=next_session['accessToken']), 200) == session['user']
        expect(server.request('POST', '/v1/auth/refresh', {'refreshToken': session['refreshToken']}), 401)
        expect(server.request('POST', '/v1/auth/refresh', {'refreshToken': next_session['refreshToken']}), 200)
        restored = sync(server, session['accessToken'], cursor=original['cursor'])
        assert restored['changes'] == updated['changes'] and restored['cursor'] == updated['cursor']
        expect(server.request('POST', '/v1/auth/login', credentials()), 200)


def edge_scenarios(binary, database):
    """Exercise decoder, routing, validation, range and session boundary behavior."""
    observed = {}
    def record(name, result, headers=()):
        selected = {k.lower(): v for k, v in result[1] if k.lower() in headers}
        data = result[2]
        if isinstance(data, dict):
            data = {k: v for k, v in data.items() if k != 'serverTime'}
        observed[name] = (result[0], selected, data)

    with running(binary, database) as server:
        session = expect(server.request('POST', '/v1/auth/register', credentials()), 201)
        token = session['accessToken']
        for method in ['POST', 'PUT', 'DELETE', 'PATCH', 'HEAD']:
            for path in ['/health', '/v1/me', '/v1/sync', '/app.js', '/v1/auth/login']:
                record((method, path), server.request(method, path), ('allow', 'content-type'))
        for path in ['/index.html', '/a/../app.js', '//app.js', '/app.js/',
                     '/v1', '/v1/', '/v1//me', '/v1/../app.js', '/%61pp.js', '/%2e%2e/app.js']:
            record(('path', path), server.request('GET', path), ('location', 'content-type', 'cache-control'))
        for range_value in ['bytes=0-9', 'bytes=-10', 'bytes=10-', 'bytes=99999999-',
                            'bytes=bad', 'items=0-1', 'bytes=10-1', 'bytes=0-0,2-2']:
            result = server.request('GET', '/icon.svg', headers={'Range': range_value})
            if result[0] == 206 and 'multipart/' in {k.lower():v for k,v in result[1]}.get('content-type', ''):
                # Multipart boundaries are randomly generated in the reference.
                assert b'Content-Range: bytes 0-0/' in result[2]
                assert b'Content-Range: bytes 2-2/' in result[2]
                observed[('range', range_value)] = result[0]
            else:
                record(('range', range_value), result, ('content-range', 'accept-ranges', 'content-type'))
        raw_sync = [b'null', b'{} trailing', b'{"CURSOR":null,"CHANGES":null}',
                    b'{"cursor":0,"cursor":null}', b'{"cursor":1.0}', b'[]',
                    b'{"changes":[null]}', b'{"deviceId":false}', b'{"pull":0}',
                    b'{"changes":[],"changes":null}', b'{"cursor":9223372036854775808}']
        for n, raw in enumerate(raw_sync):
            record(('raw sync', n), server.request('POST', '/v1/sync', token=token, raw=raw))
        cases = [change('x', entity='Category'), change('x'*129),
                 change('x', payload=None), change('x', payload=[1,2,3]),
                 change('x', stamp='0001-01-01T00:00:00Z'),
                 change('x', stamp='2025-01-01t00:00:00z'),
                 change('x', stamp='2025-01-01T0:00:00Z'),
                 change('x', stamp='2025-01-01T00:00:60Z'),
                 change('x', stamp='2025-01-01T00:00:00,123456789123Z'),
                 change('x', stamp='2025-01-01T00:00:00+24:00'),
                 change('x', stamp='2025-01-01T00:00:00+00:60'),
                 change('x', stamp='2025-01-01T00:00:00.123456789123Z'),
                 change('x', stamp='9999-01-01T00:00:00Z'),
                 change('x', payload='x'*(256*1024))]
        for n, value in enumerate(cases):
            record(('sync validation', n), server.request('POST', '/v1/sync', {'changes':[value]}, token))
        for n, raw in enumerate([b'null', b'{} tail', b'{"refreshToken":null}', b'{"REFRESHTOKEN":"bad"}', b'{"refreshToken":1}', b'{"extra":1}']):
            record(('refresh JSON', n), server.request('POST', '/v1/auth/refresh', raw=raw))
            record(('logout JSON', n), server.request('POST', '/v1/auth/logout', raw=raw))
        for auth in ['', 'Basic bad', 'bearer bad', 'Bearer ', 'Bearer bad']:
            record(('bearer', auth), server.request('GET', '/v1/me', headers={'Authorization':auth}))
        # Verify signed JWTs with claims from Go's permissive JSON decoder.
        for n, claims in enumerate([
            {'sub':'u','email':'e','iss':'balance-server','exp':4102444800},
            {'SUB':'u','EMAIL':'e','ISS':'balance-server','EXP':4102444800},
            {'sub':'u','email':None,'iss':'balance-server','exp':4102444800},
            {'sub':'u','iss':'balance-server','exp':4102444800,'unknown':123},
            {'sub':'u','iss':'wrong','exp':4102444800},
            {'sub':'u','iss':'balance-server','exp':1},
            {'sub':'','iss':'balance-server','exp':4102444800},
        ]):
            encode = lambda data: base64.urlsafe_b64encode(data).rstrip(b'=')
            unsigned = b'ignored-header.' + encode(json.dumps(claims).encode())
            signed = (unsigned+b'.'+encode(hmac.new(SECRET.encode(), unsigned, hashlib.sha256).digest())).decode()
            record(('signed claims', n), server.request('GET', '/v1/me', token=signed))
        # Only one simultaneous rotation of a given refresh session may succeed.
        fresh = expect(server.request('POST','/v1/auth/login',credentials()),200)
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            statuses = sorted(pool.map(lambda _: server.request('POST','/v1/auth/refresh',{'refreshToken':fresh['refreshToken']})[0], range(4)))
        assert statuses == [200,401,401,401], statuses
    with running(binary, database) as server:
        for n, raw in enumerate([b'null', b'{"email":null,"password":null}',
                                b'{"EMAIL":"bad","PASSWORD":"abcdefgh"}',
                                b'{"email":"bad","email":null}', b'{"email":false}',
                                b'{"email":"bad"} trailing', b'{"email":"bad","unknown":1}']):
            for endpoint in ['register','login']:
                record((endpoint, n), server.request('POST', '/v1/auth/'+endpoint, raw=raw))
        for n, value in enumerate([credentials('a@b'),dict(email='utf8@example.com',password='я'*4),
                                  dict(email='long@example.com',password='x'*1025)]):
            result = server.request('POST','/v1/auth/register',value)
            if result[0] == 201:
                assert expect(server.request('POST','/v1/auth/login',value),200)['user'] == result[2]['user']
                observed[('password boundary',n)] = result[0]
            else:
                record(('password boundary',n), result)
    with running(binary,database,BALANCE_REFRESH_TTL='1ms') as server:
        session = expect(server.request('POST','/v1/auth/login',credentials()),200)
        time.sleep(.05)
        record('expired refresh',server.request('POST','/v1/auth/refresh',{'refreshToken':session['refreshToken']}))
    return observed


def raw_json_scenarios(binary, database):
    """RawMessage must retain numbers, duplicate keys and unusual Unicode bytes."""
    observed = {}
    with running(binary, database) as server:
        session = expect(server.request('POST','/v1/auth/register',credentials()),201)
        token = session['accessToken']
        cursor = 0
        for n, payload in enumerate([
            b'{ "duplicate":1, "duplicate":2, "n":123456789012345678901234567890, "big":1e999 }',
            br'{"lone":"\ud800","low":"\udfff","pair":"\ud83d\ude00"}',
            b'{"invalid":"\xff\xc0\xed\xa0\x80"}',
            b'{"html":"<>&","separator":"\xe2\x80\xa8\xe2\x80\xa9"}',
            br'{"nested":[{"payload":{"id":"inner"}},"quote\"end",null,true]}',
        ]):
            raw = (b'{"cursor":'+str(cursor).encode()+b',"changes":[{"entity":"transaction","id":"wire-'+str(n).encode()+
                   b'","updatedAt":"2025-01-01T00:00:00Z","payload":'+payload+b'}]}')
            result = server.request('POST','/v1/sync',token=token,raw=raw,decode_json=False)
            assert result[0] == 200, result
            data = result[2]
            # Latin-1 preserves byte indices while Python locates the complete JSON value.
            start = data.index(b'"payload":') + len(b'"payload":')
            _, end = json.JSONDecoder().raw_decode(data[start:].decode('latin-1'))
            observed[('wire payload',n)] = data[start:start+end]
            decoded = json.loads(data.decode('utf-8','replace'))
            cursor = decoded['cursor']
            assert len(decoded['changes']) == 1
        for n, raw in enumerate([br'{"email":"\ud800","password":"abcdefgh"}',
                                 b'{"email":"\xff@example.com","password":"abcdefgh"}']):
            # Login has deterministic invalid credentials and exercises scalar normalization.
            result = server.request('POST','/v1/auth/login',raw=raw)
            observed[('unicode credentials',n)] = (result[0],result[2])
        for n, raw_id in enumerate([br'"\ud800"',b'"\xff"']):
            raw = (b'{"cursor":'+str(cursor).encode()+b',"changes":[{"entity":"category","id":'+raw_id+
                   b',"updatedAt":"2025-01-02T00:00:00Z","payload":{}}]}')
            result = server.request('POST','/v1/sync',token=token,raw=raw)
            assert result[0] == 200, result
            data = result[2]
            data.pop('serverTime')
            observed[('unicode id',n)] = data
            cursor = data['cursor']
    with sqlite3.connect(database) as db:
        db.text_factory = bytes
        observed['raw database payloads'] = db.execute('SELECT record_id,payload FROM records ORDER BY record_id').fetchall()
    return observed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--go-binary', type=Path)
    parser.add_argument('--rust-binary', type=Path, default=ROOT / 'target/debug/balance-server')
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix='balance-compatibility-') as directory:
        directory = Path(directory)
        go = args.go_binary.resolve() if args.go_binary else directory / 'go-server'
        if args.go_binary is None:
            subprocess.run(['go','build','-o',str(go),'./cmd/balance-server'], cwd=ROOT, check=True)
        rust = args.rust_binary.resolve()
        assert rust.is_file(), f'Build Rust first: cargo build --locked (missing {rust})'
        print('Running Go reference scenarios...', flush=True)
        expected = scenario(go, directory / 'go.db')
        print('Running Rust scenarios...', flush=True)
        actual = scenario(rust, directory / 'rust.db')
        for key in expected:
            assert actual[key] == expected[key], f'Differential mismatch: {key}\nGo: {expected[key]!r}\nRust: {actual[key]!r}'
        print('Comparing HTTP/JSON edge cases...', flush=True)
        expected = edge_scenarios(go, directory / 'go-edge.db')
        actual = edge_scenarios(rust, directory / 'rust-edge.db')
        for key in expected:
            assert actual[key] == expected[key], f'Differential mismatch: {key}\nGo: {expected[key]!r}\nRust: {actual[key]!r}'
        print('Comparing raw JSON payload and Unicode preservation...', flush=True)
        expected = raw_json_scenarios(go, directory / 'go-raw.db')
        actual = raw_json_scenarios(rust, directory / 'rust-raw.db')
        for key in expected:
            assert actual[key] == expected[key], f'Differential mismatch: {key}\nGo: {expected[key]!r}\nRust: {actual[key]!r}'
        for first, second, name in [(go,rust,'go-rust'),(rust,go,'rust-go')]:
            print(f'Checking {name} SQLite takeover and rollback...', flush=True)
            takeover(first, second, directory / f'{name}.db')
    print('PASS: HTTP differential scenarios and bilateral SQLite takeover/rollback')


if __name__ == '__main__':
    main()
