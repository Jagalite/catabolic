// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import createClient from "openapi-fetch";
import type { paths, components } from "./generated/rest.js";
import type { CatalogQuery, CatalogQueryVariables } from "./generated/graphql.js";

// Credentials enter through stdin, never command arguments or logs.
const fixture = JSON.parse(readFileSync(0, "utf8"));
const client = createClient<paths>({
  baseUrl: fixture.base,
  headers: { Authorization: `Bearer ${fixture.token}` },
});
const anonymous = createClient<paths>({ baseUrl: fixture.base });
const denied = await anonymous.GET("/v1/me");
assert.equal(denied.response.status, 401);
assert.equal(denied.error?.code, "unauthorized");
const identity = await client.GET("/v1/me");
assert.equal(identity.data?.principal, "website");
const caps = await client.GET("/v1/capabilities");
assert.equal(caps.data?.version, 1);
const items = await client.GET("/v1/items", { params: { query: { limit: 1 } } });
assert.equal(items.data?.data[0]?.id, fixture.state.item_id);
assert.equal(items.data?.has_more, false);
const snapshot = await client.POST("/v1/items/snapshots");
assert.ok(snapshot.data);
const page = await client.GET("/v1/snapshots/{identifier}", {
  params: { path: { identifier: snapshot.data.snapshot_id }, query: { limit: 1 } },
});
assert.equal(page.data?.data[0]?.id, fixture.state.item_id);
const variables: CatalogQueryVariables = { first: 1 };
const graphql = await client.POST("/v1/query/graphql", {
  body: { query: readFileSync(new URL("../catalog.graphql", import.meta.url), "utf8"), variables },
});
assert.equal(graphql.data?.errors, undefined);
// The operation-specific type is generated from SDL and the query document.
const document = graphql.data?.data as CatalogQuery | undefined;
assert.equal(document?.items?.nodes[0]?.id, fixture.state.item_id);
assert.equal(typeof document?.files?.nodes[0]?.size, "string");
const saved = await client.POST("/v1/queries/{revision_id}/runs", {
  params: { path: { revision_id: fixture.query_id } }, body: {},
});
assert.ok(saved.data && "ids" in saved.data);
assert.deepEqual(saved.data.ids, [fixture.state.item_id]);
const sql = await client.POST("/v1/query/sql", { body: { query: "SELECT 1" } });
assert.equal(sql.error?.code, "forbidden");
const demand = await client.POST("/v1/rendition-requests", {
  params: { header: { "Idempotency-Key": "generated-client" } }, body: fixture.state,
});
assert.ok(demand.data, JSON.stringify(demand.error));
assert.ok([200, 202].includes(demand.response.status));
const replay = await client.POST("/v1/rendition-requests", {
  params: { header: { "Idempotency-Key": "generated-client" } }, body: fixture.state,
});
assert.equal(replay.data?.request_id, demand.data.request_id);
const result = await client.GET("/v1/rendition-requests/{identifier}", {
  params: { path: { identifier: demand.data.request_id } },
});
assert.equal(result.data?.state, "ready");
assert.ok(result.data.result);
const ready: components["schemas"]["RenditionResult"] = result.data.result;
const file = await client.GET("/v1/files/{identifier}", {
  params: { path: { identifier: ready.file_id } },
});
assert.equal(file.data?.revision, ready.revision);
assert.equal(typeof file.data?.size, "string");
assert.equal(file.data?.path, undefined);
const ticket = await client.POST("/v1/content-access", {
  body: { file_id: ready.file_id, revision: ready.revision },
});
assert.ok(ticket.data);
const ticketUrl = new URL(ticket.data.content_path, fixture.base);
const query = { revision: ready.revision, ticket: ticketUrl.searchParams.get("ticket")! };
const bytes = await anonymous.GET("/v1/files/{identifier}/content", {
  params: { path: { identifier: ready.file_id }, query, header: { Range: "bytes=0-7" } },
  parseAs: "arrayBuffer",
});
assert.equal(bytes.response.status, 206);
assert.deepEqual(Buffer.from(bytes.data!), Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]));
const head = await anonymous.HEAD("/v1/files/{identifier}/content", {
  params: { path: { identifier: ready.file_id }, query },
});
assert.equal(head.response.status, 200);
const original = await client.GET("/v1/files/{identifier}/content", {
  params: { path: { identifier: fixture.state.source_file_id }, query: { revision: fixture.state.source_revision } },
});
assert.equal(original.error?.code, "not_found");
const revoked = await client.POST("/v1/content-access/{identifier}/revoke", {
  params: { path: { identifier: ticket.data.ticket_id } },
});
assert.equal(revoked.data?.revoked, true);
const after = await anonymous.GET("/v1/files/{identifier}/content", {
  params: { path: { identifier: ready.file_id }, query },
});
assert.equal(after.error?.code, "unauthorized");
const events = await client.GET("/v1/events", { params: { query: { follow: false } } });
assert.ok(events.data && typeof events.data !== "string");
assert.ok(events.data.events.some(event => event.state === "ready"));
console.log("Generated REST and GraphQL clients: authenticated catalog, request, tickets and exact bytes passed.");
