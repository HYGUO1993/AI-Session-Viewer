import type { ImportResult, SkillsResult } from "../types";
import {
  getViewerNodes,
  LOCAL_NODE_ID,
  type ViewerNode,
} from "./nodeConfig";

declare const __IS_TAURI__: boolean;

export function getSkillSyncNodes(): ViewerNode[] {
  const nodes = getViewerNodes();
  if (__IS_TAURI__) return nodes;

  const currentOrigin = window.location.origin;
  return [
    {
      id: LOCAL_NODE_ID,
      name: "当前服务器",
      baseUrl: currentOrigin,
      token: localStorage.getItem("asv_token") ?? "",
    },
    ...nodes.filter((node) => node.baseUrl !== currentOrigin),
  ];
}

async function nodeFetch(
  node: ViewerNode,
  path: string,
  init?: RequestInit,
): Promise<Response> {
  const headers = new Headers(init?.headers);
  if (node.token) headers.set("Authorization", `Bearer ${node.token}`);
  const response = await fetch(new URL(path, node.baseUrl), { ...init, headers });
  if (response.status === 401) throw new Error(`${node.name} 的访问令牌无效`);
  if (!response.ok) {
    const message = await response.text();
    throw new Error(message || `${node.name} 请求失败 (${response.status})`);
  }
  return response;
}

export async function listNodeGlobalSkills(node: ViewerNode): Promise<SkillsResult> {
  return (await nodeFetch(node, "/api/skills")).json();
}

export async function exportNodeGlobalSkill(
  node: ViewerNode,
  slug: string,
): Promise<ArrayBuffer> {
  const path = `/api/skills/sync-export?slug=${encodeURIComponent(slug)}`;
  return (await nodeFetch(node, path)).arrayBuffer();
}

export async function applyNodeGlobalSkill(
  node: ViewerNode,
  slug: string,
  archive: ArrayBuffer,
  overwrite: boolean,
): Promise<ImportResult> {
  const path =
    `/api/skills/sync-apply?slug=${encodeURIComponent(slug)}` +
    `&overwrite=${String(overwrite)}`;
  return (
    await nodeFetch(node, path, {
      method: "POST",
      headers: { "Content-Type": "application/octet-stream" },
      body: archive,
    })
  ).json();
}
