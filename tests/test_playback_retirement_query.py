# SPDX-FileCopyrightText: 2026 The Catabolic Contributors
# SPDX-License-Identifier: MIT
"""Compare the production candidate SQL with an independent bounded ledger."""
import itertools
import re
import sqlite3
import unittest
from pathlib import Path

SOURCE = Path(__file__).resolve().parents[1] / 'crates/catabolic-store/src/playback_cache.rs'


class PlaybackRetirementQuery(unittest.TestCase):
    def test_candidate_query_respects_live_demand_and_retirement_boundaries(self):
        source = SOURCE.read_text()
        match = re.search(r'"(SELECT j\.\* FROM api_playback_jobs j WHERE j\.profile=.*?LIMIT 20)"', source)
        self.assertIsNotNone(match, 'production retirement candidate query changed')
        db = sqlite3.connect(':memory:')
        self.addCleanup(db.close)
        db.executescript('''
            CREATE TABLE api_playback_jobs(id TEXT,profile TEXT,state TEXT,directory_inode INTEGER,updated_at REAL,created_at REAL);
            CREATE TABLE api_playback_sessions(job_id TEXT,cancelled INTEGER,expires REAL);
        ''')
        states = ('queued', 'running', 'ready', 'failed', 'stale')
        # None: no session; cancelled: live but cancelled; active: live demand;
        # expired: uncancelled but expired exactly at the observation time.
        sessions = ('none', 'cancelled', 'active', 'expired')
        for state, directory, session, updated in itertools.product(states, (False, True), sessions, (399.0, 400.0, 401.0)):
            db.execute('DELETE FROM api_playback_sessions')
            db.execute('DELETE FROM api_playback_jobs')
            db.execute('INSERT INTO api_playback_jobs VALUES (?, ?, ?, ?, ?, ?)', ('j', 'p', state, 1 if directory else None, updated, 0))
            if session != 'none':
                db.execute('INSERT INTO api_playback_sessions VALUES (?, ?, ?)', ('j', int(session == 'cancelled'), 4000.0 if session == 'expired' else 4001.0))
            found = bool(db.execute(match.group(1), ('p', 4000.0, 400.0)).fetchall())
            expected = state != 'running' and (state != 'stale' or directory) and session != 'active' and (state != 'ready' or updated < 400.0)
            self.assertEqual(found, expected, (state, directory, session, updated))
        # Negative control: omitting the live-demand predicate must make an
        # active session appear eligible, which contradicts the ledger.
        mutated = re.sub(r' AND NOT EXISTS\(SELECT 1 FROM api_playback_sessions.*?\)', '', match.group(1))
        self.assertNotEqual(mutated, match.group(1))
        db.execute('DELETE FROM api_playback_sessions')
        db.execute('DELETE FROM api_playback_jobs')
        db.execute("INSERT INTO api_playback_jobs VALUES ('j','p','ready',1,0,0)")
        db.execute("INSERT INTO api_playback_sessions VALUES ('j',0,4001)")
        self.assertFalse(db.execute(match.group(1), ('p', 4000.0, 400.0)).fetchall())
        self.assertTrue(db.execute(mutated, ('p', 400.0)).fetchall())


if __name__ == '__main__':
    unittest.main()
