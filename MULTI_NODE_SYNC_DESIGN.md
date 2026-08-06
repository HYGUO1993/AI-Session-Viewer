# 多机访问与配置同步设计

## 目标

让一个 AI Session Viewer 客户端管理多个 `session-web` 节点，并在明确选择源节点和目标节点后同步 Skill、MCP 声明和插件安装声明。

首版不做跨节点统计聚合，也不同步会话、API Key、认证文件或插件缓存。

## 现状结论

### 多机访问

- Tauri 模式的 `api.ts` 固定调用本机 IPC；Web 模式固定请求 `window.location.origin`。
- `session-web` 已支持 `ASV_HOST`、Bearer Token、WebSocket 单次票据和跨域请求，可以作为远程节点。
- HTTP、文件监听 WebSocket、CLI 对话 WebSocket 和认证令牌目前都没有“当前节点”参数，不能只靠页面下拉框完成切换。
- 所有 provider 都从运行 `session-web` 的机器读取 Home 目录。Web 模式只是查看服务器本机数据，不是多节点聚合。

### Skill、MCP 和插件

- Skill 目前只扫描 Claude：`~/.claude/skills`、项目 `.claude/skills` 和 `~/.claude/plugins/{marketplaces,cache}`。
- Skill 已有 ZIP 导入能力，但没有导出清单/归档接口，也没有 Codex Skill 根目录模型。
- MCP 没有读取、预览、写入或脱敏逻辑。
- 插件只读扫描 marketplace/cache 中包含的 Skill；没有插件安装清单 API。cache 是派生数据，不应跨机复制。

### 使用统计

- 日期筛选原先由前端按本地日期生成、后端按 UTC 日期截取，跨日统计会错位。
- Claude 会话数原先只收集带 usage 的 assistant 行，会漏掉只有 user 行的会话。
- 区间模型分布原先按全历史比例估算；未知模型费用按 `$0` 展示。

上述统计问题已在本次修改中修复，并统一使用用户选择的 IANA 时区；默认跟随系统时区。

## 节点模型

节点配置只保存在当前客户端，不上传到任何节点：

```ts
interface ViewerNode {
  id: string;
  name: string;
  baseUrl: string;
  token?: string;
}
```

- Tauri 保留一个不可删除的“本机”节点，使用现有 IPC。
- Web 保留一个不可删除的“当前服务器”节点，使用页面 origin。
- 远程节点 URL 必须是 `http://` 或 `https://`，保存前去掉末尾 `/`。
- Token 按节点保存，不复用单一的 `asv_token`。
- 切换节点后关闭旧 WebSocket，清空项目、会话、消息、搜索、统计、账单和聊天状态，再从新节点加载。

## 节点通信

所有需要节点数据的传输都从同一份活动节点配置解析目标：

```text
活动节点
  ├─ 本机 Tauri -> tauriApi / IPC / Tauri event
  └─ session-web -> webApi / HTTP / WebSocket / Bearer Token
```

不能只修改普通 `fetch`：以下路径必须同时切换。

- 通用 GET/POST/PUT/DELETE
- 导出和 Skill ZIP 的二进制请求
- `/ws` 文件监听
- `/ws/chat` CLI 对话
- `/api/auth/ws-ticket` 票据签发
- 401 后的节点级 Token 更新

节点状态首版复用轻量认证探测接口；后续可增加 `/api/node-info` 返回节点名、版本和能力，不用扫描会话目录。

## 同步资源模型

同步必须是显式的“源节点 -> 目标节点”，不提供隐式双向同步。

```text
读取源清单 -> 选择资源 -> 目标预检 -> 冲突预览 -> 备份 -> 写入 -> 校验
```

### Skill

支持范围：

- Claude 全局 Skill
- Codex 全局 Skill
- 用户明确映射了同一路径的项目 Skill

同步单位是完整 Skill 目录，不只复制 `SKILL.md`。源节点生成 ZIP，目标节点复用现有安全解压和 slug 校验。

冲突策略：`skip | overwrite`。覆盖前将原目录备份到应用配置目录；符号链接默认物化为归档内容，不在目标机创建指向源机路径的链接。

### MCP

使用结构化 JSON/TOML 解析器读取 Claude/Codex 支持的 MCP 声明，只同步服务器名称、命令、参数和非敏感环境变量。

以下值默认脱敏且不进入同步包：

- key 名匹配 `TOKEN|SECRET|PASSWORD|API_KEY|PRIVATE_KEY|AUTH`
- Bearer、Basic 或 URL 内嵌凭据
- 指向 `auth.json`、凭据文件、私钥文件的内容

预览必须逐项显示“新增、覆盖、跳过、需要目标机补值”。用户只有显式勾选后才能传输敏感值。

### 插件

只同步 marketplace 声明和已安装插件的逻辑标识/版本约束；目标节点调用自己的插件安装流程重建文件。

禁止同步：

- `~/.claude/plugins/cache`
- 下载产物和临时目录
- 机器绝对路径
- 插件运行态、认证信息和 API Key

在项目尚未提供稳定的插件安装清单解析与安装 API 前，插件同步保持只读预览，不提供“复制 cache”替代方案。

## API 草案

```text
GET  /api/node-info
GET  /api/sync/manifest?kind=skills|mcp|plugins
POST /api/sync/export
POST /api/sync/preview
POST /api/sync/apply
GET  /api/sync/jobs/{id}
```

`preview` 和 `apply` 使用同一份带哈希的清单。若目标文件在预览后发生变化，`apply` 必须拒绝执行，避免覆盖并发修改。

所有同步 API 复用 `session-web` Bearer Token 认证。远程节点未启用 Token 时，UI 必须显示安全警告；应用不代替 TLS，公网节点仍要求 HTTPS 或可信反向代理。

## 实施顺序

1. 节点注册、连接测试、选择和在线状态。
2. 统一 HTTP/WebSocket/认证路由，并处理 Tauri 本机与远程节点切换。
3. Skill 扫描扩展到 Claude/Codex，增加归档导出、预览、备份和校验。
4. MCP 结构化读取、脱敏清单和显式冲突处理。
5. 插件安装声明探测；确认各版本格式后再开放写入。

当前已完成 1、2、Claude 全局 Skill 同步、MCP 结构化脱敏同步，以及插件安装声明只读预览。MCP 首版不开放敏感值传输，只保留目标机已有值或提示目标机补值；Codex Skill、项目 Skill 路径映射和插件声明写入仍未开放。

每一步都可独立交付。跨节点统计聚合只有在单节点统计口径稳定且用户明确需要时再增加。

## 验收标准

- 客户端能保存多个节点并显示连接状态，切换后所有 HTTP 和 WebSocket 都访问同一节点。
- 节点切换不会混用上一个节点的项目、统计、聊天或 Token。
- 同步前能看到源、目标、资源差异、冲突策略和脱敏项。
- 写入前自动备份；写入后按哈希校验；失败时不留下半写入文件。
- 会话、API Key、认证文件和插件 cache 永远不进入默认同步包。
- 首版统计只展示当前节点，不把多机数据静默相加。
