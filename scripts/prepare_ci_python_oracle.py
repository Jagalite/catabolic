#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Build the Linux XML oracle from checksum-pinned Python and Expat sources."""

import argparse
import hashlib
import json
import os
import platform
import shlex
import subprocess
import tarfile
import urllib.request
from pathlib import Path

SOURCES = {
    "Python-3.14.6.tar.xz": (
        "https://www.python.org/ftp/python/3.14.6/Python-3.14.6.tar.xz",
        "143b1dddefaec3bd2e21e3b839b34a2b7fb9842272883c576420d605e9f30c63",
    ),
    "expat-2.8.2.tar.xz": (
        "https://github.com/libexpat/libexpat/releases/download/R_2_8_2/expat-2.8.2.tar.xz",
        "3ad89b8588e6644bd4e49981480d48b21289eebbcd4f0a1a4afb1c29f99b6ab4",
    ),
}


def download(url, expected, destination):
    with urllib.request.urlopen(url, timeout=120) as response:
        data = response.read()
    if hashlib.sha256(data).hexdigest() != expected:
        raise RuntimeError(f"source checksum mismatch: {url}")
    destination.write_bytes(data)


def runtime_identity(executable, prefix):
    result = subprocess.run(
        [
            str(executable),
            "-I",
            "-B",
            "-c",
            "import json,platform,ssl,sqlite3,sys; from xml.parsers import expat; "
            "print(json.dumps(dict(version=list(sys.version_info[:3]), "
            "expat=expat.EXPAT_VERSION, system=platform.system(), "
            "prefix=sys.prefix, executable=sys.executable)))",
        ],
        check=True,
        capture_output=True,
        text=True,
        timeout=30,
    )
    identity = json.loads(result.stdout)
    if (
        identity["version"] != [3, 14, 6]
        or identity["expat"] != "expat_2.8.2"
        or identity["system"] != "Linux"
        or Path(identity["prefix"]) != prefix
        or Path(identity["executable"]) != executable
    ):
        raise RuntimeError(f"built oracle identity mismatch: {identity}")
    identity["executable_sha256"] = hashlib.sha256(executable.read_bytes()).hexdigest()
    return identity


def configured_expat(makefile, archive):
    settings = [
        line.partition("=")[2].strip()
        for line in makefile.read_text().splitlines()
        if line.partition("=")[0].strip() == "MODULE_PYEXPAT_LDFLAGS"
    ]
    if len(settings) != 1 or shlex.split(settings[0]) != [str(archive), "-lm"]:
        raise RuntimeError(
            f"configure did not select the pinned Expat archive: {settings}"
        )


def build(prefix, work):
    # Fresh directories prevent an ambient installation from receiving a receipt.
    prefix.mkdir(parents=True, exist_ok=False)
    work.mkdir(parents=True, exist_ok=False)
    for name, (url, digest) in SOURCES.items():
        archive = work / name
        download(url, digest, archive)
        with tarfile.open(archive) as stream:
            stream.extractall(work, filter="data")

    def run(args, cwd=work, env=None):
        subprocess.run([str(arg) for arg in args], cwd=cwd, env=env, check=True)

    # A PIC static Expat keeps pyexpat bound to this version in child venvs,
    # without changing the runner's loader search path or system packages.
    expat_build = work / "expat-build"
    run(
        [
            "cmake",
            "-S",
            work / "expat-2.8.2",
            "-B",
            expat_build,
            f"-DCMAKE_INSTALL_PREFIX={prefix}",
            "-DCMAKE_BUILD_TYPE=Release",
            "-DCMAKE_INSTALL_LIBDIR=lib",
            "-DCMAKE_POSITION_INDEPENDENT_CODE=ON",
            "-DEXPAT_SHARED_LIBS=OFF",
            "-DEXPAT_BUILD_TESTS=OFF",
            "-DEXPAT_BUILD_EXAMPLES=OFF",
            "-DEXPAT_BUILD_TOOLS=OFF",
            "-DEXPAT_BUILD_DOCS=OFF",
        ]
    )
    run(["cmake", "--build", expat_build, "--parallel", "2"])
    run(["cmake", "--install", expat_build])
    python_source = work / "Python-3.14.6"
    env = os.environ.copy()
    for key in ("LD_LIBRARY_PATH", "PYTHONHOME", "PYTHONPATH"):
        env.pop(key, None)
    env.update(
        LIBEXPAT_CFLAGS=f"-I{prefix}/include",
        LIBEXPAT_LDFLAGS=f"{prefix}/lib/libexpat.a -lm",
        # setup-python adds its own libpython to LD_LIBRARY_PATH. DT_RPATH
        # keeps the new executable and its child venvs on the library we built.
        LDFLAGS=f"-Wl,--disable-new-dtags -Wl,-rpath,{prefix}/lib",
    )
    run(
        [
            python_source / "configure",
            f"--prefix={prefix}",
            "--enable-shared",
            "--with-system-expat",
            "--enable-loadable-sqlite-extensions",
            "--with-ensurepip=install",
        ],
        cwd=python_source,
        env=env,
    )
    configured_expat(python_source / "Makefile", prefix / "lib/libexpat.a")
    run(["make", "-j2"], cwd=python_source, env=env)
    run(["make", "install"], cwd=python_source, env=env)
    executable = prefix / "bin/python"
    executable.symlink_to("python3.14")
    return runtime_identity(executable, prefix)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prefix", required=True, type=Path)
    parser.add_argument("--work-dir", required=True, type=Path)
    parser.add_argument("--receipt", required=True, type=Path)
    args = parser.parse_args()
    receipt = args.receipt.absolute()
    receipt.parent.mkdir(parents=True, exist_ok=True)
    report = {"passed": False, "sources": SOURCES}
    receipt.write_text(json.dumps(report, indent=2) + "\n")
    if platform.system() != "Linux":
        raise RuntimeError("the frozen XML codec oracle requires Linux")
    report["runtime"] = build(args.prefix.absolute(), args.work_dir.absolute())
    report["passed"] = True
    receipt.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
