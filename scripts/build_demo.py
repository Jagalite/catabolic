# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Build a static demo from real owner services on disposable generated media."""

import argparse
import copy
import json
import shutil
import subprocess
import tempfile
from pathlib import Path

import catabolic
from catabolic import components
from catabolic.app import Application
from catabolic.curation import Curation, occurrence
from catabolic.fallback_policies import Policies
from catabolic.fallback_projection import FallbackProjection
from catabolic.fallback_resolution import Resolver
from catabolic.graphql_query import execute_graphql
from catabolic.item_workflow import ItemWorkflow
from catabolic.layouts import PRESETS, Layouts
from catabolic.processing import Processing
from catabolic.saved_queries import Queries
from catabolic.sql_query import execute_sql
from catabolic.store import Store

ROOT = Path(__file__).resolve().parents[1]


def build(destination):
    if destination.exists():
        raise ValueError("Choose a new output directory")
    destination.mkdir(parents=True)
    for name in ("index.html", "style.css", "app.js"):
        shutil.copyfile(ROOT / "demo" / name, destination / name)
    shutil.copyfile(
        ROOT / "docs/assets/catabolic-banner.png", destination / "banner.png"
    )
    (destination / ".nojekyll").touch()
    with tempfile.TemporaryDirectory(prefix="catabolic-demo-") as temporary:
        root = Path(temporary).resolve()
        for folder in ("studio", "archive", "output"):
            (root / folder).mkdir()
        subtitle = root / "studio" / "Orbit.en.forced.srt"
        subtitle.write_text(
            "1\n00:00:00,000 --> 00:00:00,800\nA small world, carefully kept.\n",
            encoding="utf-8",
        )
        subprocess.run(
            [
                "ffmpeg",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=0x246ccf:s=160x90:d=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
                "-i",
                str(subtitle),
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-map",
                "2:s",
                "-c:v",
                "mpeg4",
                "-c:a",
                "aac",
                "-c:s",
                "srt",
                "-metadata:s:a:0",
                "language=eng",
                "-metadata:s:s:0",
                "language=eng",
                "-disposition:s:0",
                "forced",
                str(root / "studio/Orbit.mkv"),
            ],
            check=True,
            timeout=60,
        )
        shutil.copyfile(root / "studio/Orbit.mkv", root / "archive/Orbit.mkv")
        (root / "studio/Field-notes.txt").write_text(
            "Observations from the Orbit expedition.\n"
        )
        extra_movies = [
            ("Glass Harbor", 2024, "Drama"),
            ("Signal North", 2025, "Science Fiction"),
            ("The Last Orchard", 1998, "Drama"),
            ("Paper Moonlight", 1987, "Comedy"),
            ("Deep Current", 2023, "Documentary"),
            ("Winter Radio", 2004, "Drama"),
            ("Small Revolutions", 2026, "Documentary"),
            ("Red Valley", 1994, "Adventure"),
            ("Parallel Days", 2025, "Science Fiction"),
            ("Sunday Assembly", 2012, "Comedy"),
        ]
        for title, _year, _genre in extra_movies:
            for source in ("studio", "archive"):
                shutil.copyfile(
                    root / "studio/Orbit.mkv", root / source / (title + ".mkv")
                )
        episodes = []
        for show in ("North Station", "The Quiet Signal"):
            for season in (1, 2):
                for episode in (1, 2, 3):
                    filename = f"{show}.S{season:02d}E{episode:02d}.mkv"
                    episodes.append((show, season, episode, filename))
                    for location in ("studio", "archive"):
                        shutil.copyfile(
                            root / "studio/Orbit.mkv", root / location / filename
                        )
        database = root / "catalog.sqlite3"

        Store.initialize(database)
        with Store(database, writable=True) as store:
            app = Application(store)
            for source in ("studio", "archive"):
                app.bind("source", source, str(root / source))
            app.bind("output", "global", str(root / "output"))
            report = app.scan()
            assert report["complete"], report
            files = {
                (r["location"], r["path"]): r["id"]
                for r in store.rows("SELECT * FROM files")
            }
            queries = {
                "files": (
                    "File occurrences",
                    "SELECT f.path,f.location,o.status,o.size FROM files f JOIN observations o ON o.file_id=f.id WHERE o.profile=:profile ORDER BY f.location,f.path",
                ),
                "inbox": (
                    "Outstanding work",
                    "SELECT work_key,category,label,reason,actionability FROM catalog_work_inbox WHERE profile=:profile AND actionability IN ('ready','blocked','needs_decision') ORDER BY work_key",
                ),
                "components": (
                    "English audio + forced subtitles",
                    "SELECT kind,language,forced,storage,file_id,occurrence_id FROM catalog_component_occurrences WHERE profile=:profile AND current=1 AND language='en' AND (kind='audio' OR (kind='subtitle' AND forced=1)) ORDER BY kind,occurrence_id",
                ),
            }
            for key, label, condition in [
                ("audio", "English audio", "kind='audio' AND language='en'"),
                (
                    "subtitles",
                    "Forced English subtitles",
                    "kind='subtitle' AND language='en' AND forced=1",
                ),
                ("external", "External components", "storage='external'"),
            ]:
                queries[key] = (
                    label,
                    "SELECT kind,language,codec,forced,storage,file_id FROM catalog_component_occurrences WHERE profile=:profile AND current=1 AND "
                    + condition
                    + " ORDER BY kind,file_id",
                )
            snapshots = []

            def sql(query):
                result = execute_sql(database, query, _store=store, max_rows=200)
                assert result["complete"], result
                return result

            def capture(key, title, description, command, evidence=None):
                snapshots.append(
                    dict(
                        id=key,
                        title=title,
                        description=description,
                        command=command,
                        queries={
                            name: dict(label=label, sql=query, result=sql(query))
                            for name, (label, query) in queries.items()
                        },
                        evidence=evidence or {},
                        outputs=[
                            dict(
                                path=str(p.relative_to(root / "output")),
                                target=str(p.resolve()).replace(str(root), "/demo"),
                            )
                            for p in sorted((root / "output").rglob("*"))
                            if p.is_symlink()
                        ],
                    )
                )

            capture(
                "discover",
                "Start with what is there.",
                "Two sources. Four file occurrences. No placeholder items. A scan records discoveries; the inbox makes unidentified files visible.",
                "catabolic scan\ncatabolic inbox list",
                report,
            )
            curation = Curation(app)
            original = files[("studio", "Orbit.mkv")]
            proposal = curation.put(
                original,
                {
                    "item": {
                        "kind": "movie",
                        "identities": {"demo": "orbit"},
                        "metadata": {"title": "Orbit", "year": 2026},
                    }
                },
            )
            capture(
                "propose",
                "A suggestion, awaiting a decision.",
                "The identification proposal owns its pending decision. Listing the inbox does not accept it.",
                "catabolic proposal put FILE_ID --file proposal.json",
                proposal,
            )
            curation.decide(proposal["id"], accept=True, actor="demo:operator")
            item = store.rows(
                "SELECT item_id FROM item_files WHERE file_id=?", (original,)
            )[0]["item_id"]
            app.media.associate(files[("archive", "Orbit.mkv")], item)
            app.media.associate(
                files[("studio", "Orbit.en.forced.srt")],
                item,
                role="subtitle",
                metadata={"language": "en", "forced": True},
            )
            workflow = ItemWorkflow(app)
            review = workflow.require(
                item, "review", "Confirm edition and component compatibility"
            )
            capture(
                "review",
                "Identity is only the beginning.",
                "The movie is identified. An explicit review requirement now asks the operator to confirm its edition and accompanying tracks. The field notes remain unidentified.",
                'catabolic item require ITEM_ID --kind review --label "Confirm edition and component compatibility"',
                review,
            )
            for title, year, genre in extra_movies:
                proposal = curation.put(
                    files[("studio", title + ".mkv")],
                    {
                        "item": {
                            "kind": "movie",
                            "identities": {"demo": title},
                            "metadata": {"title": title, "year": year, "genre": genre},
                        }
                    },
                )
                curation.decide(proposal["id"], accept=True, actor="demo:operator")
                extra_item = store.rows(
                    "SELECT item_id FROM item_files WHERE file_id=?",
                    (files[("studio", title + ".mkv")],),
                )[0]["item_id"]
                app.media.associate(files[("archive", title + ".mkv")], extra_item)
            for show in ("North Station", "The Quiet Signal"):
                app.put_item("series", {}, {"title": show}, "demo-show-" + show)
                for season in (1, 2):
                    sid = f"demo-season-{show}-{season}"
                    app.put_item("season", {}, {"title": f"Season {season}"}, sid)
                    app.media.relate(
                        sid, "demo-show-" + show, "part_of", position=season
                    )
            for show, season, episode, filename in episodes:
                eid = f"demo-episode-{show}-{season}-{episode}"
                app.put_item(
                    "episode",
                    {},
                    {
                        "title": f"{show} - S{season:02d}E{episode:02d}",
                        "year": 2025,
                        "genre": "Drama",
                    },
                    eid,
                )
                app.media.relate(
                    eid, f"demo-season-{show}-{season}", "part_of", position=episode
                )
                for location in ("studio", "archive"):
                    app.media.associate(files[(location, filename)], eid)
            processing = Processing(app)
            processing.enqueue(
                "probe",
                file_ids=[
                    fid
                    for (location, path), fid in files.items()
                    if path.endswith((".mkv", ".srt"))
                ],
            )
            probes = processing.run(workers=1)
            assert probes["complete"], probes
            # The sidecar was the exact subtitle input used to encode this fixture.
            from catabolic.component_sql import OCCURRENCES_SQL

            sidecar = store.rows(
                "SELECT * FROM (" + OCCURRENCES_SQL + ") WHERE file_id=? AND current=1",
                (files[("studio", "Orbit.en.forced.srt")],),
            )[0]
            revision = components.revision(occurrence(store, "default", original))
            assertion = curation.put(
                sidecar["file_id"],
                {
                    "item_id": item,
                    "component": {
                        "occurrence_id": sidecar["occurrence_id"],
                        "compatibility": {
                            "edition_id": item,
                            "video_file_id": original,
                            "video_revision": revision,
                            "part": None,
                            "timeline_id": "container:" + revision,
                            "coverage": "full",
                            "offset_seconds": 0.0,
                            "synchronization": "verified",
                        },
                    },
                },
                evidence={
                    "demo": "Sidecar is the exact generated subtitle used in the container; same timeline."
                },
            )
            curation.decide(assertion["id"], accept=True, actor="demo:operator")
            workflow.resolve(
                review["requirement_id"],
                "complete",
                note="Generated edition and subtitle timing reviewed",
            )
            workflow.set_status(item, "complete")

            def saved(name, query):
                return Queries(store).put(
                    name, {"selection": {"language": "sql", "query": query}}
                )["id"]

            tiers = [
                {
                    "name": source,
                    "query_id": saved(
                        source,
                        "SELECT id AS file_id FROM files WHERE location='"
                        + source
                        + "' AND path LIKE '%.mkv'",
                    ),
                }
                for source in ("studio", "archive")
            ]
            audio = saved(
                "english-audio",
                "SELECT occurrence_id FROM catalog_component_occurrences WHERE profile=:profile AND kind='audio' AND language='en' AND current=1",
            )
            embedded = saved(
                "embedded-forced",
                "SELECT occurrence_id FROM catalog_component_occurrences WHERE profile=:profile AND kind='subtitle' AND language='en' AND forced=1 AND current=1 AND storage='embedded'",
            )
            external = saved(
                "external-forced",
                "SELECT occurrence_id FROM catalog_component_occurrences WHERE profile=:profile AND kind='subtitle' AND language='en' AND forced=1 AND current=1 AND storage='external'",
            )
            policies = {}
            for name, subtitle_query, publication in [
                ("container", embedded, "container"),
                ("sidecars", external, "sidecars"),
            ]:
                policies[name] = Policies(store).put(
                    "demo-" + name,
                    {
                        "fallbacks": tiers,
                        "failback": {"mode": "immediate"},
                        "package": {
                            "publication": publication,
                            "requirements": [
                                {
                                    "name": "audio",
                                    "fallbacks": [
                                        {"name": "English", "query_id": audio}
                                    ],
                                },
                                {
                                    "name": "subtitles",
                                    "fallbacks": [
                                        {
                                            "name": "Forced English",
                                            "query_id": subtitle_query,
                                        }
                                    ],
                                },
                            ],
                        },
                    },
                )["id"]
            packages = {
                name: Resolver(app).resolve([{"item_id": item}], policy)
                for name, policy in policies.items()
            }
            capture(
                "select",
                "Choose components. Then packaging.",
                "The same occurrence model finds embedded and external tracks. These two saved plans select English audio and forced English subtitles, then decide whether to use the container or publish a compatible sidecar. A container link still exposes all its tracks.",
                "catabolic query run QUERY_ID\ncatabolic fallback resolve --help",
                packages,
            )
            membership = saved(
                "movie-membership", "SELECT id AS item_id FROM items WHERE kind='movie'"
            )
            Layouts(app).put("demo-flat", copy.deepcopy(PRESETS["flat"]))
            projection = FallbackProjection(app)
            projection.bind("global", policies["container"], membership, "demo-flat")
            preview = projection.run("global")
            capture(
                "preview",
                "See the destination before writing.",
                "Catabolic plans the output from saved membership, fallback and layout definitions. This recorded preview creates no links.",
                "catabolic projection plan global",
                preview,
            )
            applied = projection.run(
                "global", apply=True, expected_plan=preview["plan_id"]
            )
            assert applied["complete"], applied
            capture(
                "publish",
                "One maintained library.",
                "The approved plan publishes a symlink through the existing ownership and journal machinery. Source files stay in place.",
                "catabolic projection execute global\ncatabolic verify",
                applied,
            )
            output_layouts = [
                ("plex", "Plex", copy.deepcopy(PRESETS["plex-v1"])),
                ("native", "Catabolic native", copy.deepcopy(PRESETS["catabolic"])),
                (
                    "years",
                    "By year",
                    {
                        "version": 1,
                        "rules": [
                            {
                                "name": "all",
                                "path": "{item.year}/{item.title}/{file.name}",
                            }
                        ],
                    },
                ),
                (
                    "flat",
                    "Flat files",
                    {
                        "version": 1,
                        "rules": [
                            {
                                "name": "all",
                                "path": "{item.title} ({item.year}){file.extension}",
                            }
                        ],
                    },
                ),
            ]
            trees = {}
            selection_queries = {}
            for key, label, condition in [
                ("all", "All media", "kind IN ('movie','episode')"),
                (
                    "recent",
                    "Recent picks",
                    """kind IN ('movie','episode')
  AND json_extract(metadata,'$.year') >= 2020
  AND json_extract(metadata,'$.genre') IN ('Drama', 'Science Fiction')""",
                ),
                (
                    "components",
                    "English + subs",
                    """kind IN ('movie','episode')
  AND EXISTS (
    SELECT 1 FROM catalog_component_occurrences c
    WHERE c.item_id=items.id AND c.profile=:profile
      AND c.current=1 AND c.technically_verified=1
      AND c.kind='audio' AND c.language='en'
  )
  AND EXISTS (
    SELECT 1 FROM catalog_component_occurrences c
    WHERE c.item_id=items.id AND c.profile=:profile
      AND c.current=1 AND c.technically_verified=1
      AND c.kind='subtitle' AND c.language='en'
      AND c.forced=1 AND c.storage='external'
  )""",
                ),
                (
                    "backed_up",
                    "Two sources",
                    """kind IN ('movie','episode')
  AND 2 = (
    SELECT count(DISTINCT f.location)
    FROM item_files a
    JOIN files f ON f.id=a.file_id
    JOIN observations o ON o.file_id=f.id
    WHERE a.item_id=items.id AND a.active=1 AND a.role='primary'
      AND o.profile=:profile AND o.status='present'
      AND f.location IN ('studio', 'archive')
  )""",
                ),
            ]:
                statement = (
                    "SELECT id AS item_id\nFROM items\nWHERE "
                    + condition
                    + "\nORDER BY id"
                )
                selection_queries[key] = {
                    "label": label,
                    "sql": statement,
                    "result": sql(statement),
                }
                graphql = (
                    '{ movies: items(kind: "movie", first: 100) { nodes { id title year } pageInfo { hasNextPage } } episodes: items(kind: "episode", first: 100) { nodes { id title year } pageInfo { hasNextPage } } }'
                    if key == "all"
                    else None
                )
                if graphql:
                    graphql_result = execute_graphql(database, graphql, _store=store)
                    assert not graphql_result.get("errors"), graphql_result
                    pages = list(graphql_result["data"].values())
                    assert all(not page["pageInfo"]["hasNextPage"] for page in pages)
                    assert {n["id"] for page in pages for n in page["nodes"]} == {
                        r[0] for r in selection_queries[key]["result"]["rows"]
                    }
                    selection_queries[key]["graphql"] = graphql
                else:
                    selection_queries[key]["graphql_note"] = (
                        "This saved selection uses SQL. The GraphQL item filters do not express this condition directly."
                    )
                query_id = saved("selection-" + key, statement)
                for layout_key, layout_label, definition in output_layouts:
                    name = key + "-" + layout_key
                    folder = root / ("projection-" + name)
                    folder.mkdir()
                    app.bind("output", name, str(folder))
                    Layouts(app).put("demo-" + name, definition)
                    projection.bind(
                        name, policies["container"], query_id, "demo-" + name
                    )
                    plan = projection.run(name)
                    published = projection.run(
                        name, apply=True, expected_plan=plan["plan_id"]
                    )
                    assert published["complete"], published
                    trees[name] = {
                        "label": layout_label,
                        "files": [
                            {
                                "path": str(p.relative_to(folder)),
                                "target": str(p.resolve()).replace(str(root), "/demo"),
                            }
                            for p in sorted(folder.rglob("*"))
                            if p.is_symlink()
                        ],
                    }
            (root / "studio").rename(root / "studio-offline")
            failover = projection.run("global")
            result = projection.run(
                "global", apply=True, expected_plan=failover["plan_id"]
            )
            assert result["complete"], result
            capture(
                "offline",
                "A source disappears. The library adapts.",
                "The same saved fallback policy rejects the unavailable studio source and selects the archive container with its compatible tracks. The logical movie remains the same.",
                "catabolic projection plan global\ncatabolic projection execute global",
                result,
            )
            offline_packages = Resolver(app).resolve(
                [{"item_id": item}], policies["container"]
            )
            data = dict(
                explorer={
                    "layouts": [
                        {"id": key, "label": label} for key, label, _ in output_layouts
                    ],
                    "trees": trees,
                    "selections": selection_queries,
                    "queries": snapshots[3]["queries"],
                    "projections": {
                        "container": packages["container"],
                        "sidecars": packages["sidecars"],
                        "offline": offline_packages,
                    },
                },
                version=catabolic.__version__,
                scenarios=snapshots,
                provenance={
                    "kind": "recorded-real-operations",
                    "generator": "scripts/build_demo.py",
                    "source": subprocess.check_output(
                        ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
                    ).strip(),
                    "fixture": "Generated 1-second video/audio, embedded and external English subtitles, and a text document. No production media.",
                },
            )
            encoded = json.dumps(data, indent=2).replace(str(root), "/demo")
            (destination / "demo.json").write_text(encoded + "\n")
            assert len(snapshots) == 7
    print(f"Built {len(snapshots)} recorded scenarios in {destination}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    build(parser.parse_args().output.resolve())
