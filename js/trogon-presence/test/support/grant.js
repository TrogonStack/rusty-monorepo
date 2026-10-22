import { ConnectionId, Topic } from "../../src/values.js";

/**
 * Builds a `fetchGrant` callback for one (tenant, sub, sid) application session: the real
 * contract treats a topic-set change as the only reason to mint a fresh connection (the
 * `enroll` command), and reuses the existing connection for a plain pre-expiry or
 * post-disconnect refresh (the `refresh_session` command behind the `refresh` RPC).
 *
 * @param {import("./rpc.js").RpcClient} rpc
 * @param {{ tenant: string, sub: string, sid: string, sessionSecs: number }} identity
 */
export function createFetchGrant(rpc, identity) {
  let connectionId = null;
  let topicsKey = null;

  return async function fetchGrant(topics) {
    const key = topics
      .map((topic) => topic.toString())
      .sort()
      .join("\u0000");

    if (connectionId != null && key === topicsKey) {
      const reply = await rpc.call("refresh", {
        tenant: identity.tenant,
        sub: identity.sub,
        sid: identity.sid,
        connection_id: connectionId,
        session_secs: identity.sessionSecs,
      });
      connectionId = reply.connection_id;
      return { token: reply.token, connectionId: new ConnectionId(connectionId), deniedTopics: [] };
    }

    const reply = await rpc.call("enroll", {
      tenant: identity.tenant,
      sub: identity.sub,
      sid: identity.sid,
      topics: topics.map((topic) => topic.raw),
      session_secs: identity.sessionSecs,
    });
    connectionId = reply.connection_id;
    topicsKey = key;
    const deniedTopics = reply.denied.map((denial) => ({
      topic: new Topic(denial.topic),
      reason: denial.reason,
    }));
    return { token: reply.token, connectionId: new ConnectionId(connectionId), deniedTopics };
  };
}
