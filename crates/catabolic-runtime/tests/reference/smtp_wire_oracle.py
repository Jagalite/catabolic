# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Offline pinned SMTP oracle. No Python is used by native delivery.

SMTP and SMTP_SSL are replaced before constructing providers. Socket creation
is denied, so no destination here can reach an external service. All credentials
and addresses are synthetic. MIME comparisons omit Date, Message-ID, boundary,
header folding and recipient set ordering, which are inherently nondeterministic.
"""
import contextlib
import io
import json
import logging
import socket
from email import policy
from email.parser import Parser
from email.utils import getaddresses
from unittest.mock import patch

import apprise
from apprise.plugins.base import NotifyBase


CASES = [
    "mailto://user:synthetic@example.com/one@example.net",
    "mailto://user@example.com",
    "mailto://localhost",
    "mailtos://user:synthetic@example.com/one@example.net?mode=ssl",
    "mailtos://user:synthetic@example.com/one@example.net",
    "mailto://127.0.0.1:12345/one@example.net?from=sender@example.com",
    "mailto://user:synthetic@example.com/One:one@example.net/Two:two@example.net?cc=one@example.net,copy@example.net,hidden@example.net&bcc=hidden@example.net&reply=help@example.net&name=Service",
    "mailto://user:synthetic@example.com/one@example.net?to=two@example.net&format=text",
    "mailto://user:synthetic@example.com/one@example.net?format=markdown",
    "mailto://user:synthetic@example.com/one@example.net?from=%C3%89lodie%20%3Csender@example.com%3E",
    "mailto://user:synthetic@example.com/Person%20Name:one@example.net?cc=Copy:copy@example.net&reply=Help:help@example.net",
    "mailto://user:synthetic@example.com/Person%20Name%20one@example.net",
    "mailto://user:synthetic@example.com/%27Person%20Name%27%3Aone@example.net",
    "mailto://user:synthetic@example.com/one@example.net%20two@example.net",
    "mailto://old@example.com?user=new%40origin.net&pass=p+q%252B",
    "mailto://synthetic@example.com?user=person",
    "mailto://user%40origin.net:synthetic@example.com",
    "mailto://user:synthetic@gmail.com",
    "mailto://user:synthetic@gmail.com?mode=insecure&smtp=127.0.0.1&port=12345",
    "mailto://user:synthetic@yahoo.com",
    "mailto://user:synthetic@yandex.ru",
    "mailto://user:synthetic@hotmail.com",
    "mailto://user:synthetic@outlook.com",
    "mailto://user:synthetic@office365.com",
    "mailto://user:synthetic@gmx.de",
    "mailto://user:synthetic@fastmail.com",
    "mailto://user:synthetic@mm.st",
    "mailto://user:synthetic@zoho.com",
    "mailto://user:synthetic@sendgrid.net",
    "mailto://user:synthetic@163.com",
    "mailto://user:synthetic@qq.com",
    "mailto://user:synthetic@comcast.net",
    "mailto://user:synthetic@example.com?retry=1&wait=0&optional=yes&cto=1&rto=2",
    "mailto://old:@example.com?user=new",
    # Internationalized mailboxes. Apprise 1.13.1 has no SMTPUTF8 path: its
    # address validator drops them (To) or rejects the URL (From), and it never
    # passes the SMTPUTF8 MAIL option to smtplib.
    "mailto://user:synthetic@example.com/j%C3%B3se@example.net",
    "mailto://user:synthetic@example.com/one@ex%C3%A4mple.net",
    "mailto://user:synthetic@example.com/one@example.net/j%C3%B3se@example.net",
    "mailto://user:synthetic@example.com/one@example.net?from=s%C3%A9@example.com",
    "mailto://user:synthetic@example.com/one@example.net?cc=j%C3%B3se@example.net&reply=r%C3%A9@example.net",
]

# Apprise 1.13.1 prepare_emails changes reply_to from set to list at line 1269.
# With two To recipients, the second subtraction at line 1253 raises TypeError.
# Keep its false result and the first emitted message as frozen evidence.
KNOWN_REFERENCE_BUGS = {CASES[6]: "reply_to_set_mutation"}

# Apprise admits these URLs but drops every invalid To target, logs "There are
# no Email recipients to notify" and returns False without connecting.
NATIVE_EXPECTATIONS = {CASES[34]: "no_recipients", CASES[35]: "no_recipients"}


def reject(*args, **kwargs):
    raise AssertionError("Network is forbidden in the frozen SMTP oracle")


assert apprise.__version__ == "1.13.1", apprise.__version__
logging.disable(logging.CRITICAL)
records = []
for url in CASES:
    messages = []

    class SMTP:
        def __init__(self, *args, **kwargs):
            pass

        def login(self, *args):
            pass

        def starttls(self, *args, **kwargs):
            pass

        def quit(self):
            pass

        def sendmail(self, sender, recipients, body):
            parsed = Parser(policy=policy.default).parsestr(body)
            messages.append({
                "sender": sender,
                "recipients": sorted(address for _, address in getaddresses(recipients)),
                "headers": {name.lower(): [[name, addr] for name, addr in getaddresses(parsed.get_all(name, []))]
                            for name in ["From", "To", "Cc", "Reply-To", "Bcc"]},
                "subject": str(parsed["Subject"]),
                "application": str(parsed["X-Application"]),
                "parts": [{"type": part.get_content_type(), "body": part.get_payload(decode=True).decode(part.get_content_charset() or "ascii")}
                          for part in parsed.walk() if not part.is_multipart()],
            })
            return {}

    with patch("smtplib.SMTP", SMTP), patch("smtplib.SMTP_SSL", SMTP), patch.object(socket, "socket", reject), patch.object(socket, "create_connection", reject), patch.object(NotifyBase, "throttle", lambda *args, **kwargs: None), contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
        obj = apprise.Apprise(asset=apprise.AppriseAsset(async_mode=False))
        added = obj.add(url)
        server = list(obj)[0] if added else None
        complete = obj.notify(title="Catabolic", body="A Catabolic processing job completed.", notify_type="info") if added else False
        settings = {key: getattr(server, key) for key in ["smtp_host", "port", "secure_mode", "user", "password", "from_addr", "targets", "notify_format", "retry", "wait", "optional"]} if server else None
    records.append({"url": url, "added": added, "complete": complete, "known_reference_bug": KNOWN_REFERENCE_BUGS.get(url), "native_expectation": NATIVE_EXPECTATIONS.get(url), "settings": settings, "messages": messages})
print(json.dumps({"reference": "Apprise 1.13.1; frozen Catabolic notification_worker title/body", "cases": records}, indent=2))
