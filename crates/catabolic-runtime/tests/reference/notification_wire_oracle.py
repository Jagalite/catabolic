# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Invoke the selected frozen notification owner in an independent process."""

import json
import pathlib
import sys

reference = pathlib.Path(sys.argv[1]).resolve()
sys.path.insert(0, str(reference))
from catabolic import notification_worker, notifications, store  # noqa: E402

for module in (notification_worker, notifications, store):
    assert pathlib.Path(module.__file__).resolve() == (
        reference / "catabolic" / f"{module.__name__.split('.')[-1]}.py"
    )

payload = json.load(sys.stdin)
request = payload["request"]
if payload["mapped"]:
    result = notifications.send_request(request, "wire-fixture")
else:
    result = notifications.send(
        "SYNTHETIC_UNUSED",
        "job_completed",
        "info",
        envelope=request["body"],
        url=request["url"],
    )
print(result)
