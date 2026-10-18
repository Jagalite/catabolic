# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Offline pinned SMTP AUTH transcript oracle.

Runs the already installed, pinned Apprise 1.13.1 with the real ``smtplib``
authentication logic against a scripted in-memory server. ``socket`` creation
and name resolution are denied; no network is used. The credentials and RFC 2195
challenge are public synthetic examples. The scripted server behaviour must stay
identical to the Rust test server in ``notifications/smtp.rs``.
"""

import base64
import contextlib
import io
import json
import logging
import socket
import smtplib
from unittest.mock import patch

import apprise
from apprise.plugins.base import NotifyBase

USER = "tim"
PASSWORD = "tanstaaftanstaaf"
# RFC 2195 section 2 example challenge and expected digest.
CHALLENGE = b"<1896.697170952@postoffice.reston.mci.net>"

# name, advertised AUTH words, mechanisms the server accepts
SCENARIOS = [
    ("cram_preferred_over_plain_and_login", "PLAIN LOGIN CRAM-MD5", ["CRAM-MD5", "PLAIN", "LOGIN"]),
    ("cram_rejected_falls_back_to_plain", "CRAM-MD5 PLAIN LOGIN", ["PLAIN", "LOGIN"]),
    ("cram_and_plain_rejected_falls_back_to_login", "CRAM-MD5 PLAIN LOGIN", ["LOGIN"]),
    ("cram_before_login_without_plain", "LOGIN CRAM-MD5", ["CRAM-MD5", "LOGIN"]),
    ("cram_only_rejected_fails", "CRAM-MD5", []),
    ("plain_and_login_unchanged", "PLAIN LOGIN", ["PLAIN", "LOGIN"]),
    ("login_only_unchanged", "LOGIN", ["LOGIN"]),
    ("unknown_mechanisms_fail", "XOAUTH2 NTLM", ["XOAUTH2"]),
]


def reject(*args, **kwargs):
    raise AssertionError("Network is forbidden in the frozen SMTP AUTH oracle")


class Server:
    """Replies exactly like the Rust fixture; records client command lines."""

    def __init__(self, advertised, accepted):
        self.advertised = advertised
        self.accepted = accepted
        self.out = io.BytesIO()
        self.pending = b""
        self.replies = [b"220 scripted fixture\r\n"]
        self.state = None
        self.commands = []
        self.in_data = False

    def feed(self, data):
        self.pending += data
        while b"\r\n" in self.pending:
            line, self.pending = self.pending.split(b"\r\n", 1)
            self.line(line.decode("ascii"))

    def say(self, text):
        self.replies.append(text.encode("ascii") + b"\r\n")

    def verdict(self, mechanism):
        self.state = None
        self.say("235 authenticated" if mechanism in self.accepted else "535 authentication failed")

    def line(self, line):
        if self.in_data:
            if line == ".":
                self.in_data = False
                self.say("250 queued")
            return
        self.commands.append(line)
        state, upper = self.state, line.upper()
        if state == "cram":
            self.verdict("CRAM-MD5")
        elif state == "login-user":
            self.state = "login-pass"
            self.say("334 UGFzc3dvcmQ6")
        elif state == "login-pass":
            self.verdict("LOGIN")
        elif upper.startswith("EHLO"):
            self.say("250-scripted fixture")
            self.say("250-SIZE 262144")
            self.say("250 AUTH " + self.advertised)
        elif upper == "AUTH CRAM-MD5":
            self.state = "cram"
            self.say("334 " + base64.b64encode(CHALLENGE).decode())
        elif upper.startswith("AUTH PLAIN "):
            self.verdict("PLAIN")
        elif upper.startswith("AUTH LOGIN "):
            self.state = "login-pass"
            self.say("334 UGFzc3dvcmQ6")
        elif upper.startswith("AUTH "):
            self.say("504 mechanism unavailable")
        elif upper == "DATA":
            self.in_data = True
            self.say("354 start data")
        elif upper == "QUIT":
            self.say("221 bye")
        else:
            self.say("250 ok")


class Reader:
    def __init__(self, server):
        self.server = server

    def readline(self, limit=-1):
        if not self.server.replies:
            return b""
        return self.server.replies.pop(0)

    def close(self):
        pass


class FakeSocket:
    def __init__(self, server):
        self.server = server

    def sendall(self, data):
        self.server.feed(data)

    def makefile(self, *args, **kwargs):
        return Reader(self.server)

    def settimeout(self, value):
        pass

    def close(self):
        pass


assert apprise.__version__ == "1.13.1", apprise.__version__
logging.disable(logging.CRITICAL)
records = []
for name, advertised, accepted in SCENARIOS:
    server = Server(advertised, accepted)
    url = f"mailto://{USER}:{PASSWORD}@127.0.0.1:2525/one@example.net?from=sender@example.com&mode=insecure"
    with (
        patch.object(smtplib.SMTP, "_get_socket", lambda self, *a, **k: FakeSocket(server)),
        patch.object(socket, "socket", reject),
        patch.object(socket, "create_connection", reject),
        patch.object(socket, "getfqdn", lambda *a: "localhost"),
        patch.object(NotifyBase, "throttle", lambda *args, **kwargs: None),
        contextlib.redirect_stdout(io.StringIO()),
        contextlib.redirect_stderr(io.StringIO()),
    ):
        obj = apprise.Apprise(asset=apprise.AppriseAsset(async_mode=False))
        assert obj.add(url)
        complete = obj.notify(title="Catabolic", body="A Catabolic processing job completed.", notify_type="info")
    commands = server.commands
    mail = next((i for i, c in enumerate(commands) if c.upper().startswith("MAIL FROM")), len(commands))
    records.append(
        {
            "name": name,
            "advertised": advertised,
            "accepted": accepted,
            "complete": complete,
            # Client lines after EHLO and before MAIL FROM (the authentication exchange).
            "auth_commands": commands[1:mail],
            "mail_sent": mail < len(commands),
        }
    )
print(
    json.dumps(
        {
            "reference": "Apprise 1.13.1 with CPython smtplib; scripted in-memory server",
            "user": USER,
            "password": PASSWORD,
            "challenge_b64": base64.b64encode(CHALLENGE).decode(),
            "scenarios": records,
        },
        indent=2,
    )
)
