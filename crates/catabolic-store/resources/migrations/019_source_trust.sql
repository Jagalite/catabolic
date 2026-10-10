CREATE TABLE source_identity_policies(
 profile TEXT NOT NULL REFERENCES profiles(id), location TEXT NOT NULL REFERENCES locations(id),
 identity_policy TEXT NOT NULL CHECK(identity_policy IN ('strict','path')),
 PRIMARY KEY(profile,location));
