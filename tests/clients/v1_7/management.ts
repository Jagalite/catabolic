// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT

// The Python harness only prepares disposable catalog/auth fixtures, drives the
// native worker, and controls a loopback receiver. Every management request in
// this journey is made by the client generated from frozen HTTP 1.7.0.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import createClient from "openapi-fetch";
import type { paths, components } from "./generated/rest.js";

type Definition = Record<string, components["schemas"]["JsonValue"]>;
interface Fixture {
  phase: string;
  base: string;
  token: string;
  reader: string;
  other: string;
  receiver: string;
  item: string;
  title: string;
  identifier: string;
  definition: Definition;
  state: string;
  count: number;
  attempts?: number;
}
// Even disposable credentials enter over stdin, never command arguments.
const fixture: Fixture = JSON.parse(readFileSync(0, "utf8"));
const client = createClient<paths>({
  baseUrl: fixture.base,
  headers: { Authorization: `Bearer ${fixture.token}` },
});
const reader = createClient<paths>({
  baseUrl: fixture.base,
  headers: { Authorization: `Bearer ${fixture.reader}` },
});
const other = createClient<paths>({
  baseUrl: fixture.base,
  headers: { Authorization: `Bearer ${fixture.other}` },
});
const identifier = { path: { identifier: fixture.identifier } };
const watcher = { path: { name: "generated-management-audit" } };

async function saveQuery(name: string, definition: Definition): Promise<string> {
  const params = { path: { resource: "queries" as const, name } };
  const preview = await client.POST("/v1/operator/{resource}/{name}", {
    params, body: { definition },
  });
  assert.equal(preview.response.status, 200, JSON.stringify(preview.error));
  assert.ok(preview.data && !preview.data.applied);
  const applied = await client.POST("/v1/operator/{resource}/{name}", {
    params, body: { definition, apply: true, expected_plan: preview.data.plan_id },
  });
  assert.equal(applied.response.status, 200, JSON.stringify(applied.error));
  assert.ok(applied.data?.applied);
  assert.ok("id" in applied.data.result);
  return applied.data.result.id;
}

async function main(): Promise<unknown> {
  switch (fixture.phase) {
    case "webhook-create": {
      const body = {
        url: fixture.receiver + "/webhook?private=disposable-secret",
        events: ["projection_updated"],
      };
      const forbidden = await reader.POST("/v1/webhooks", { body });
      assert.equal(forbidden.response.status, 403);
      const invalid = await client.POST("/v1/webhooks", {
        body: { ...body, events: ["not-a-public-event"] },
      });
      assert.equal(invalid.response.status, 400);
      const created = await client.POST("/v1/webhooks", { body });
      assert.equal(created.response.status, 201, JSON.stringify(created.error));
      assert.ok(created.data?.enabled);
      assert.deepEqual(created.data.events, body.events);
      assert.equal(created.data.request_id, null);
      assert.ok(!JSON.stringify(created.data).includes("disposable-secret"));
      const params = { path: { identifier: created.data.id } };
      const inspected = await client.GET("/v1/webhooks/{identifier}", { params });
      assert.deepEqual(inspected.data, created.data);
      const denied = await other.GET("/v1/webhooks/{identifier}", { params });
      assert.equal(denied.response.status, 404);
      const isolated = await other.GET("/v1/webhooks");
      assert.deepEqual(isolated.data?.webhooks, []);
      const listing = await client.GET("/v1/webhooks");
      assert.deepEqual(listing.data?.webhooks, [created.data]);
      const history = await client.GET("/v1/webhooks/{identifier}/deliveries", { params });
      assert.deepEqual(history.data?.deliveries, []);
      return { identifier: created.data.id };
    }
    case "webhook-history": {
      const history = await client.GET("/v1/webhooks/{identifier}/deliveries", { params: identifier });
      assert.equal(history.response.status, 200, JSON.stringify(history.error));
      assert.equal(history.data?.deliveries.length, fixture.count);
      for (const row of history.data!.deliveries) {
        assert.equal(row.state, fixture.state);
        if (fixture.attempts !== undefined) assert.equal(row.attempts, fixture.attempts);
      }
      assert.ok(!JSON.stringify(history.data).includes("disposable-secret"));
      return history.data;
    }
    case "webhook-retry": {
      const result = await client.POST("/v1/webhooks/{identifier}/retry", { params: identifier });
      assert.equal(result.response.status, 200, JSON.stringify(result.error));
      assert.equal(result.data?.enabled, true);
      return {};
    }
    case "webhook-disable": {
      const disabled = await client.POST("/v1/webhooks/{identifier}/disable", { params: identifier });
      assert.equal(disabled.data?.enabled, false);
      const denied = await client.POST("/v1/webhooks/{identifier}/retry", { params: identifier });
      assert.equal(denied.response.status, 409);
      assert.equal(denied.error?.code, "webhook_disabled");
      return {};
    }
    case "mapping-create": {
      const sql = await saveQuery("generated-management-sql", {
        mode: "rows",
        selection: { language: "sql", query: "SELECT item_id AS id,title FROM catalog_items ORDER BY item_id" },
      });
      const graphql = await saveQuery("generated-management-graphql", {
        mode: "document",
        selection: {
          language: "graphql",
          query: "{items(first:100){nodes{id title} pageInfo{hasNextPage}}}",
        },
      });
      const body: components["schemas"]["HTTPOperationCreate"] = {
        name: "generated-management-receiver",
        operation_id: "notifyItem",
        base_url: fixture.receiver,
        credential_env: "CATABOLIC_MANAGEMENT_RECEIVER_TOKEN",
        spec: {
          openapi: "3.1.0", info: { title: "Disposable receiver", version: "1" },
          paths: {
            "/items/{id}/notify": {
              post: {
                operationId: "notifyItem",
                parameters: [
                  { name: "id", in: "path", required: true, schema: { type: "string" } },
                  { name: "ready", in: "query", schema: { type: "boolean" } },
                ],
                security: [{ bearer: [] }],
                requestBody: {
                  required: true,
                  content: { "application/json": { schema: { "$ref": "#/components/schemas/Event" } } },
                },
                responses: { "204": { description: "Accepted" } },
              },
            },
          },
          components: {
            securitySchemes: { bearer: { type: "http", scheme: "bearer" } },
            schemas: {
              Event: {
                type: "object", properties: { id: { type: "string" }, title: { type: "string" } },
                required: ["id", "title"], additionalProperties: false,
              },
            },
          },
        },
      };
      const forbidden = await reader.POST("/v1/http-operations", { body });
      assert.equal(forbidden.response.status, 403);
      const approved = await client.POST("/v1/http-operations", { body });
      assert.equal(approved.response.status, 201, JSON.stringify(approved.error));
      assert.ok(approved.data?.enabled);
      assert.equal(approved.data.operation_id, "notifyItem");
      const params = { path: { identifier: approved.data.id } };
      const inspect = await client.GET("/v1/http-operations/{identifier}", { params });
      assert.equal(inspect.response.status, 200);
      assert.equal(inspect.data?.id, approved.data.id);
      const denied = await other.GET("/v1/http-operations/{identifier}", { params });
      assert.equal(denied.response.status, 404);
      const isolated = await other.GET("/v1/http-operations");
      assert.deepEqual(isolated.data?.operations, []);
      const listing = await client.GET("/v1/http-operations");
      assert.deepEqual(listing.data?.operations, [approved.data]);
      const definition: Definition = {
        version: 3, id: "generated-management-sql", query: sql,
        operation: approved.data.id, key: { column: "id" },
        parameters: { path: { id: { column: "id" } }, query: { ready: { constant: true } } },
        body: { id: { column: "id" }, title: { column: "title" } },
      };
      return {
        identifier: approved.data.id,
        sql: definition,
        graphql: { ...definition, id: "generated-management-graphql", query: graphql, rows_path: ["items"] },
      };
    }
    case "mapping-apply": {
      const body = { definition: fixture.definition };
      const forbidden = await reader.POST("/v1/http-mappings/preview", { body });
      assert.equal(forbidden.response.status, 403);
      const preview = await client.POST("/v1/http-mappings/preview", { body });
      assert.equal(preview.response.status, 200, JSON.stringify(preview.error));
      assert.ok(preview.data?.complete);
      assert.equal(preview.data.applied, false);
      assert.equal(preview.data.queued, 0);
      assert.equal(preview.data.rows.length, 1);
      assert.deepEqual(preview.data.rows[0]?.request.body, { id: fixture.item, title: fixture.title });
      const applied = await client.POST("/v1/http-mappings/apply", {
        body: { ...body, expected_plan: preview.data.plan_id },
      });
      assert.equal(applied.response.status, 200, JSON.stringify(applied.error));
      assert.equal(applied.data?.applied, true);
      assert.equal(applied.data?.queued, 1);
      const again = await client.POST("/v1/http-mappings/preview", { body });
      assert.ok(again.data);
      const replay = await client.POST("/v1/http-mappings/apply", {
        body: { ...body, expected_plan: again.data.plan_id },
      });
      assert.equal(replay.response.status, 200, JSON.stringify(replay.error));
      assert.equal(replay.data?.queued, 0);
      return { plan: applied.data?.plan_id };
    }
    case "mapping-history": {
      const history = await client.GET("/v1/http-operations/{identifier}/deliveries", { params: identifier });
      assert.equal(history.response.status, 200, JSON.stringify(history.error));
      assert.equal(history.data?.deliveries.length, fixture.count);
      assert.ok(history.data?.deliveries.some(row => row.state === fixture.state));
      return history.data;
    }
    case "mapping-retry": {
      const retry = await client.POST("/v1/http-operations/{identifier}/retry", { params: identifier });
      assert.equal(retry.response.status, 200, JSON.stringify(retry.error));
      assert.equal(retry.data?.retry_scheduled, true);
      return {};
    }
    case "mapping-disable": {
      const disabled = await client.POST("/v1/http-operations/{identifier}/disable", { params: identifier });
      assert.equal(disabled.response.status, 200, JSON.stringify(disabled.error));
      assert.equal(disabled.data?.enabled, false);
      const current = await client.GET("/v1/http-operations/{identifier}", { params: identifier });
      assert.equal(current.data?.enabled, false);
      return {};
    }
    case "revoked": {
      const current = await client.GET("/v1/me");
      assert.equal(current.response.status, 401);
      assert.equal(current.error?.code, "unauthorized");
      return {};
    }
    case "watcher-create": {
      const query = await saveQuery("generated-management-watcher-query", {
        selection: { language: "sql", query: "SELECT id AS file_id FROM files ORDER BY id" },
      });
      const body: components["schemas"]["WatcherDefinition"] = {
        plan: { kind: "query", query_id: query },
      };
      const denied = await reader.PUT("/v1/operator/watchers/{name}", { params: watcher, body });
      assert.equal(denied.response.status, 403);
      const created = await client.PUT("/v1/operator/watchers/{name}", { params: watcher, body });
      assert.equal(created.response.status, 200, JSON.stringify(created.error));
      assert.equal(created.data?.enabled, false);
      assert.equal(created.data?.revision, 1);
      const again = await client.PUT("/v1/operator/watchers/{name}", { params: watcher, body });
      assert.deepEqual(again.data, created.data);
      const listing = await client.GET("/v1/operator/watchers");
      assert.deepEqual(listing.data?.watchers.map(row => row.name), [watcher.path.name]);
      const enabled = await client.POST("/v1/operator/watchers/{name}/enabled", {
        params: watcher, body: { enabled: true },
      });
      assert.equal(enabled.data?.enabled, true);
      const disabled = await client.POST("/v1/operator/watchers/{name}/enabled", {
        params: watcher, body: { enabled: false },
      });
      assert.equal(disabled.data?.enabled, false);
      const admission = await client.POST("/v1/operator/watchers/{name}/runs", { params: watcher });
      assert.equal(admission.response.status, 202);
      assert.equal(admission.data?.admitted, true);
      assert.equal(admission.data?.pending_generation, 1);
      const history = await client.GET("/v1/operator/watchers/{name}/history", { params: watcher });
      assert.deepEqual(history.data?.runs, []);
      return { name: watcher.path.name };
    }
    case "watcher-history": {
      const history = await client.GET("/v1/operator/watchers/{name}/history", { params: watcher });
      assert.equal(history.response.status, 200, JSON.stringify(history.error));
      assert.equal(history.data?.runs.length, 1);
      assert.equal(history.data?.runs[0]?.state, "complete");
      assert.equal(history.data?.runs[0]?.generation, 1);
      assert.equal(history.data?.runs[0]?.error, null);
      assert.ok(history.data?.runs[0]?.completed_at);
      const status = await client.GET("/v1/operator/watchers/{name}", { params: watcher });
      assert.equal(status.data?.active_run, null);
      assert.equal(status.data?.completed_generation, 1);
      assert.equal(status.data?.last_run, history.data?.runs[0]?.id);
      return history.data;
    }
    default: throw new Error(`Unknown management phase: ${fixture.phase}`);
  }
}

console.log(JSON.stringify(await main()));
