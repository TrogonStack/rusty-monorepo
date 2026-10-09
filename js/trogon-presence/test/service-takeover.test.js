import test from "node:test";
import assert from "node:assert/strict";

import { RpcClient, rpcAddress } from "./support/rpc.js";
import { createClient, nextCall, waitFor } from "./support/harness.js";

test("a service takeover forces a resnapshot that keeps the tracked presence intact", async (t) => {
  const rpc = new RpcClient(rpcAddress());
  t.after(() => rpc.close());
  const client = createClient(rpc, { sub: "takeover-bob" });
  t.after(() => client.close());

  const view = await client.join("rooms:service-takeover");
  await client.track(view, { status: "online" });
  await waitFor(() => Object.keys(view.state).length === 1, 5_000, "the initial track to land");

  const resyncsBefore = view.glue.resyncs;
  const resynced = nextCall((callback) => view.onSync(callback));
  await rpc.call("takeover");
  await resynced;

  assert.ok(view.glue.resyncs > resyncsBefore, "the epoch change forced a resnapshot");
  assert.deepEqual(Object.keys(view.state), ["takeover-bob"], "the tracked presence survives the new owner's snapshot");
  assert.equal(view.state["takeover-bob"].metas[0].status, "online");
});
