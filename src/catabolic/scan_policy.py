# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT

"""Owner observation policy. Coverage is independent of execution allowances."""

import json
import re
from functools import lru_cache
from pathlib import PurePosixPath

import regex

from .domain import CatabolicError, relative_path
from .store import encode

# Inventory candidates, not a claim of decodability or safe-to-delete classification.
MEDIA_EXTENSIONS = sorted(
    set(
        """
.mkv .mp4 .m4v .avi .mov .webm .mpg .mpeg .mpe .m2v .m2ts .mts .ts .vob
.ogv .wmv .asf .flv .f4v .3gp .3g2 .divx .rm .rmvb .mxf
.mp3 .flac .wav .wave .aac .m4a .m4b .m4p .mka .ogg .oga .opus .aif .aiff
.alac .ape .wv .wma .dsf .dff .ac3 .eac3 .dts .mid .midi .amr .au
.srt .ass .ssa .vtt .sub .idx .sup .smi .sami .ttml .dfxp .scc .lrc
.jpg .jpeg .png .gif .webp .bmp .tif .tiff .heic .heif .avif .jxl .svg
.raw .dng .cr2 .cr3 .nef .arw .orf .rw2 .raf .pef .srw .psd
.epub .pdf .mobi .azw .azw3 .azw4 .fb2 .djvu .cbz .cbr .cb7 .cbt
.txt .md .rtf .doc .docx .odt .html .htm
.nfo .xml .xmp .cue .m3u .m3u8 .pls .asx .log .ttf .otf .woff .woff2
.iso .img .bin .dat .ifo .bup .bdmv .mpls .clpi .zip .rar .7z
""".split()
    )
)


def extensions(value):
    """None explicitly requests all regular files; lists replace the default set."""
    if value is None:
        return None
    if not isinstance(value, list) or not value or len(value) > 1000:
        raise CatabolicError("extensions must be a nonempty list or null for all files")
    if any(
        not isinstance(v, str) or not re.fullmatch(r"\.[a-zA-Z0-9]{1,20}", v)
        for v in value
    ):
        raise CatabolicError(
            "extensions must be suffixes such as .mkv; globs are not supported"
        )
    return sorted({v.lower() for v in value})


@lru_cache(maxsize=128)
def compile_pattern(pattern):
    return regex.compile(pattern, regex.VERSION0)


def patterns(value):
    if not isinstance(value, list) or len(value) > 32:
        raise CatabolicError("regex filters must be lists of at most 32 patterns")
    for pattern in value:
        if not isinstance(pattern, str) or not 1 <= len(pattern) <= 1024:
            raise CatabolicError("regex patterns must contain 1 to 1024 characters")
        try:
            compile_pattern(pattern)
        except (regex.error, OverflowError, RecursionError) as exc:
            raise CatabolicError(f"invalid scan regex: {exc}") from exc
    return sorted(set(value))


def included(path, allowed, include_regex=(), exclude_regex=()):
    if allowed is not None and PurePosixPath(path).suffix.lower() not in allowed:
        return False
    try:
        if include_regex and not any(
            compile_pattern(p).search(path, timeout=0.01) for p in include_regex
        ):
            return False
        return not any(
            compile_pattern(p).search(path, timeout=0.01) for p in exclude_regex
        )
    except TimeoutError as exc:
        from .scan_traversal import ScanResourceStop

        raise ScanResourceStop("regex_filter_timeout") from exc


DEFAULTS = dict(
    directory_entries=50000,
    depth=64,
    directory_seconds=15,
    metadata_seconds=2,
    errors=16,
    total_seconds=120,
    total_entries=250000,
    pending_directories=100000,
    batch_records=500,
    batch_bytes=1048576,
    batch_seconds=1,
    temporary_bytes=67108864,
    minimum_free_bytes=67108864,
)
EXTENDED = dict(
    directory_entries=1000000,
    depth=4096,
    directory_seconds=300,
    metadata_seconds=10,
    errors=64,
    total_seconds=1800,
    total_entries=5000000,
)


def limits(values=None):
    values = {} if values is None else values
    if not isinstance(values, dict) or set(values) - set(DEFAULTS):
        raise CatabolicError("unknown scan budget")
    if any(type(v) not in (int, float) or not 0 < v < 10**12 for v in values.values()):
        raise CatabolicError("scan budgets must be finite positive numbers")
    integer_keys = set(DEFAULTS) - {
        "directory_seconds",
        "metadata_seconds",
        "total_seconds",
        "batch_seconds",
    }
    if any(k in integer_keys and type(v) is not int for k, v in values.items()):
        raise CatabolicError("scan count and byte budgets must be integers")
    result = {**DEFAULTS, **values}
    # Even extended requests keep hard memory bounds.
    if result["batch_records"] > 10000 or result["batch_bytes"] > 16777216:
        raise CatabolicError("scan batch exceeds hard memory allowance")
    return result


def effective(app, source):
    rows = app.store.rows(
        "SELECT * FROM source_observation_policies WHERE profile=? AND source=?",
        (app.profile, source),
    )
    row = rows[0] if rows else {}
    policy = json.loads(row.get("policy", "{}"))
    return dict(
        revision=row.get("revision", 0),
        exclusions=policy.get("exclusions", []),
        extensions=extensions(policy.get("extensions", MEDIA_EXTENSIONS)),
        include_regex=patterns(policy.get("include_regex", [])),
        exclude_regex=patterns(policy.get("exclude_regex", [])),
        budgets=limits(policy.get("budgets")),
    )


def configure(app, source, policy=None, *, apply=False):
    app.binding("source", source)
    old = effective(app, source)
    if policy is None:
        return old
    if not isinstance(policy, dict) or set(policy) - {
        "exclusions",
        "budgets",
        "extensions",
        "include_regex",
        "exclude_regex",
    }:
        raise CatabolicError("invalid observation policy")
    exclusions = policy.get("exclusions", old["exclusions"])
    if not isinstance(exclusions, list):
        raise CatabolicError("exclusions must be a list")
    new = dict(
        exclusions=sorted({relative_path(p) for p in exclusions}),
        extensions=extensions(policy.get("extensions", old["extensions"])),
        include_regex=patterns(policy.get("include_regex", old["include_regex"])),
        exclude_regex=patterns(policy.get("exclude_regex", old["exclude_regex"])),
        budgets=limits(policy.get("budgets", old["budgets"])),
    )
    changed = any(old[k] != new[k] for k in new)
    revision = old["revision"] + int(changed)
    if apply and changed:
        with app.store.transaction() as db:
            db.execute(
                "INSERT INTO source_observation_policies VALUES (?,?,?,?) ON CONFLICT(profile,source) DO UPDATE SET revision=excluded.revision,policy=excluded.policy",
                (app.profile, source, revision, encode(new)),
            )
            db.execute(
                "INSERT INTO source_observation_policy_history(profile,source,revision,policy) VALUES (?,?,?,?)",
                (app.profile, source, revision, encode(new)),
            )
            from .source_events import invalidate

            invalidate(app, source, reason="observation_policy_changed")
    return dict(previous=old, policy=new, revision=revision, applied=apply)


def execution(app, source, *, extended=False, budgets=None, scopes=None):
    policy = effective(app, source)
    merged = {**policy["budgets"], **(EXTENDED if extended else {}), **(budgets or {})}
    return dict(
        budgets=limits(merged),
        scopes=sorted({relative_path(p) if p else "" for p in (scopes or [""])}),
        policy_revision=policy["revision"],
        extensions=policy["extensions"],
        include_regex=policy["include_regex"],
        exclude_regex=policy["exclude_regex"],
    )
