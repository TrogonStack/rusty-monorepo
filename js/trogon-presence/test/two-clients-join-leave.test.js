import test from "node:test";
import assert from "node:assert/strict";

import { RpcClient, rpcAddress } from "./support/rpc.js";
import { createClient, nextCall } from "./support/harness.js";

test("two clients see each other join and leave the same topic", async (t) => {
  const rpc = new RpcClient(rpcAddress());
  t.after(() => rpc.close());
  const watcher = createClient(rpc, { sub: "two-clients-watcher" });
  t.after(() => watcher.close());
  const writer = createClient(rpc, { sub: "two-clients-writer" });
  t.after(() => writer.close());

  const watcherView = await watcher.join("rooms:two-clients");
  const writerView = await writer.join("rooms:two-clients");

  const joined = nextCall((callback) => watcherView.onJoin(callback));
  await writer.track(writerView, { status: "online" });
  const [joinedKey, before, after] = await joined;
  assert.equal(joinedKey, "two-clients-writer");
  assert.equal(before, undefined);
  assert.equal(after.metas[0].status, "online");
  assert.deepEqual(Object.keys(watcherView.state), ["two-clients-writer"]);

  const left = nextCall((callback) => watcherView.onLeave(callback));
  await writer.untrack(writerView);
  const [leftKey, remaining] = await left;
  assert.equal(leftKey, "two-clients-writer");
  assert.deepEqual(remaining.metas, []);
  assert.deepEqual(Object.keys(watcherView.state), [], "the writer's leave clears the watcher's view");
});
