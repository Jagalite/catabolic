# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Offline-only frozen provider wire oracle. Never used by the Rust runtime.

Requires the already pinned Apprise 1.13.1 installation. No packages are fetched.
requests.post is replaced before any provider is created. Socket creation is
blocked so a missed monkeypatch fails closed. Credentials are synthetic.
"""
import contextlib
import io
import json
import logging
import socket
from unittest.mock import patch
import apprise
from apprise.plugins.base import NotifyBase

CASES = [
    'ntfy://127.0.0.1:12345/alpha',
    'ntfy://old:secret@127.0.0.1:12345/topic?user=new%2540realm&pass=p%252Bq',
    'ntfy://password%252Bvalue@127.0.0.1:12345/topic?user=basic%2540realm',
    'ntfy://tk_synthetic%252B@127.0.0.1:12345/topic',
    'ntfy://127.0.0.1:12345/topic?token=tk_synthetic%252B',
    'ntfy://old:secret@127.0.0.1:12345/topic?auth=token&password=tk_synthetic%252B',
    'ntfy://%74k_synthetic@127.0.0.1:12345/topic',
    'ntfy://tk_synthetic:@127.0.0.1:12345/topic',
    'ntfy://tk_synthetic:@127.0.0.1:12345/topic?auth=token',
    'ntfy://caf%C3%A9:pa%C3%9F@127.0.0.1:12345/topic',
    'ntfy://:synthetic@127.0.0.1:12345/topic',
    'ntfy://user:synthetic@127.0.0.1:12345/topic?user=',

    'ntfy://old:secret@127.0.0.1:1/alpha?user=new&password=first&pass=replacement&port=12345',
    'ntfy://synthetic_password@127.0.0.1/alpha?user=override&port=12345&image=disable',
    'gotify://127.0.0.1:1/base/token?port=12345',
    'discord://Old@123/synthetic_token?user=New&avatar=n',

    'ntfy://user:secret@127.0.0.1:12345/zeta/alpha?priority=high&image=no&xtags=z,a&click=https%3A%2F%2Fexample.invalid%2Fx&email=box%40example.invalid&delay=30m&actions=view%2C+Open%2C+https%3A%2F%2Fexample.invalid&attach=https%3A%2F%2Fexample.invalid%2Fa&filename=file.txt',
    'ntfys://tk_synthetic@ntfy.example.invalid/a?format=markdown&avatar_url=https%3A%2F%2Fexample.invalid%2Ficon.png',
    'ntfy://synthetic_topic',
    'ntfy://ntfy.sh/alpha?mode=cloud',
    'gotify://127.0.0.1:12345/secret',
    'gotifys://gotify.example.invalid/base/secret?priority=high&format=markdown',
    'discord://123/synthetic_token',
    'discord://Robot@123/synthetic_token?tts=yes&avatar=no&format=markdown&footer=yes&image=yes&href=https%3A%2F%2Fexample.invalid&thread=345&flags=4',
    'discord://123/synthetic_token?botname=Box&ping=%3C%40123%3E&format=text',
    'tgram://123:synthetic_token/456',
    'tgram://bot123:synthetic_token/-100:23/@named_chat?silent=yes&preview=yes&format=text',
    'tgram://123%3Asynthetic_token/456?format=markdown&mdv=v2',
    'tgram://123:synthetic_token/456?format=markdown',
]

assert apprise.__version__ == '1.13.1', apprise.__version__
logging.disable(logging.CRITICAL)
records = []
def reject(*a, **k):
    raise AssertionError('Network is forbidden in the frozen wire oracle')
for url in CASES:
    for level in ['info', 'warning', 'error']:
        requests = []
        def post(url, data=None, headers=None, params=None, auth=None, **kwargs):
            from urllib.parse import urlencode
            if params:
                url += '?' + urlencode(params)
            headers = dict(headers or {})
            if auth:
                from requests.auth import _basic_auth_str
                headers['Authorization'] = _basic_auth_str(*auth)
            requests.append({'url':url,'headers':headers,'body':json.loads(data) if data else None})
            class Response:
                status_code = 200
                headers = {}
                content = b'{}'
            return Response()
        with patch('requests.post',post), patch.object(socket,'socket',reject), patch.object(NotifyBase,'throttle',lambda *a, **k:None), contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            obj=apprise.Apprise(asset=apprise.AppriseAsset(async_mode=False))
            added=obj.add(url)
            result=obj.notify(title='Catabolic',body='A Catabolic processing job completed.',notify_type={'info':'info','warning':'warning','error':'failure'}[level]) if added else False
        records.append({'url':url,'severity':level,'added':added,'complete':result,'requests':requests})
print(json.dumps({'reference':'Catabolic 453fca983222c6665a48775eef67c489c96b527e; Apprise 1.13.1','cases':records},indent=2,ensure_ascii=True))
