use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static CONFIG_WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigSyncManifest {
    pub mcp_servers: Vec<McpServerManifest>,
    pub plugins: Vec<PluginManifestItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerManifest {
    pub client: String,
    pub name: String,
    pub config: Value,
    pub redacted_fields: Vec<String>,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifestItem {
    pub client: String,
    pub kind: String,
    pub name: String,
    pub version: Option<String>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyMcpRequest {
    pub client: String,
    pub name: String,
    pub config: Value,
    pub expected_hash: String,
    #[serde(default)]
    pub overwrite: bool,
}

fn home_dir() -> Result<PathBuf, String> {
    dirs::home_dir().ok_or_else(|| "无法定位用户目录".to_string())
}

fn read_json(path: &Path) -> Result<Value, String> {
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }
    let content =
        fs::read_to_string(path).map_err(|e| format!("读取 {} 失败: {}", path.display(), e))?;
    serde_json::from_str(&content).map_err(|e| format!("解析 {} 失败: {}", path.display(), e))
}

fn read_toml(path: &Path) -> Result<toml::Value, String> {
    if !path.exists() {
        return Ok(toml::Value::Table(Default::default()));
    }
    let content =
        fs::read_to_string(path).map_err(|e| format!("读取 {} 失败: {}", path.display(), e))?;
    toml::from_str(&content).map_err(|e| format!("解析 {} 失败: {}", path.display(), e))
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_uppercase().replace('-', "_");
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "API_KEY",
        "PRIVATE_KEY",
        "AUTH",
    ]
    .iter()
    .any(|marker| key.contains(marker))
}

fn is_sensitive_string(value: &str) -> bool {
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("bearer ") || lower.contains("basic ") || is_machine_path(trimmed) {
        return true;
    }
    if let Some((_, rest)) = trimmed.split_once("://") {
        let authority = rest.split('/').next().unwrap_or(rest);
        if authority.contains('@') {
            return true;
        }
        if rest
            .split_once('?')
            .map(|(_, query)| {
                query.split('&').any(|part| {
                    part.split_once('=')
                        .map(|(key, _)| is_sensitive_key(key))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
        {
            return true;
        }
    }
    let path = lower.replace('\\', "/");
    path.ends_with("auth.json")
        || path.ends_with("credentials.json")
        || path.ends_with("id_rsa")
        || path.ends_with("id_ed25519")
        || path.ends_with(".pem")
        || path.ends_with(".key")
}

fn is_machine_path(value: &str) -> bool {
    Path::new(value).is_absolute()
        || value.as_bytes().get(1) == Some(&b':')
        || value.starts_with("\\\\")
        || value.to_ascii_lowercase().starts_with("file://")
        || value.starts_with("~/")
        || value.starts_with("~\\")
}

fn array_contains_sensitive(values: &[Value]) -> bool {
    values.iter().any(|value| {
        value.as_str().is_some_and(|text| {
            is_sensitive_string(text)
                || is_sensitive_key(text.trim_start_matches('-').replace('-', "_").as_str())
        })
    })
}

fn sanitize_value(value: &Value, path: &str, redacted: &mut Vec<String>) -> Value {
    match value {
        Value::Object(object) => {
            let mut clean = Map::new();
            for (key, value) in object {
                let field_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{}.{}", path, key)
                };
                let sensitive_value = value.as_str().is_some_and(is_sensitive_string)
                    || value
                        .as_array()
                        .is_some_and(|items| array_contains_sensitive(items));
                if is_sensitive_key(key) || sensitive_value {
                    redacted.push(field_path);
                    continue;
                }
                clean.insert(key.clone(), sanitize_value(value, &field_path, redacted));
            }
            Value::Object(clean)
        }
        Value::Array(values) => Value::Array(
            values
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    sanitize_value(value, &format!("{}[{}]", path, index), redacted)
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn sanitize_config(value: &Value) -> (Value, Vec<String>) {
    let mut redacted = Vec::new();
    let clean = sanitize_value(value, "", &mut redacted);
    (clean, redacted)
}

fn preserve_target_secrets(incoming: &mut Value, target: &Value) {
    let (Some(incoming), Some(target)) = (incoming.as_object_mut(), target.as_object()) else {
        return;
    };
    for (key, target_value) in target {
        let sensitive_value = target_value.as_str().is_some_and(is_sensitive_string)
            || target_value
                .as_array()
                .is_some_and(|items| array_contains_sensitive(items));
        if is_sensitive_key(key) || sensitive_value {
            incoming
                .entry(key.clone())
                .or_insert_with(|| target_value.clone());
        } else if let Some(incoming_value) = incoming.get_mut(key) {
            preserve_target_secrets(incoming_value, target_value);
        }
    }
}

fn value_hash(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return "missing".to_string();
    };
    let mut hasher = DefaultHasher::new();
    serde_json::to_vec(value)
        .unwrap_or_default()
        .hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn config_hash(value: Option<&Value>) -> String {
    value
        .map(|value| value_hash(Some(&sanitize_config(value).0)))
        .unwrap_or_else(|| "missing".to_string())
}

fn json_from_toml(value: &toml::Value) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| format!("转换 TOML 配置失败: {}", e))
}

fn claude_mcp_entries(path: &Path) -> Result<Vec<McpServerManifest>, String> {
    let root = read_json(path)?;
    let Some(servers) = root.get("mcpServers").and_then(Value::as_object) else {
        return Ok(Vec::new());
    };
    Ok(servers
        .iter()
        .map(|(name, config)| {
            let (config, redacted_fields) = sanitize_config(config);
            let hash = value_hash(Some(&config));
            McpServerManifest {
                client: "claude".to_string(),
                name: name.clone(),
                config,
                redacted_fields,
                hash,
            }
        })
        .collect())
}

fn codex_mcp_entries(path: &Path) -> Result<Vec<McpServerManifest>, String> {
    let root = read_toml(path)?;
    let Some(servers) = root.get("mcp_servers").and_then(toml::Value::as_table) else {
        return Ok(Vec::new());
    };
    servers
        .iter()
        .map(|(name, raw)| {
            let raw = json_from_toml(raw)?;
            let (config, redacted_fields) = sanitize_config(&raw);
            let hash = value_hash(Some(&config));
            Ok(McpServerManifest {
                client: "codex".to_string(),
                name: name.clone(),
                config,
                redacted_fields,
                hash,
            })
        })
        .collect()
}

fn read_claude_plugins(home: &Path) -> Result<Vec<PluginManifestItem>, String> {
    let plugin_dir = home.join(".claude").join("plugins");
    let installed = read_json(&plugin_dir.join("installed_plugins.json"))?;
    let marketplaces = read_json(&plugin_dir.join("known_marketplaces.json"))?;
    let mut result = Vec::new();

    if let Some(entries) = installed.get("plugins").and_then(Value::as_object) {
        for (name, installs) in entries {
            let version = installs
                .as_array()
                .and_then(|items| {
                    items
                        .iter()
                        .find_map(|item| item.get("version").and_then(Value::as_str))
                })
                .map(str::to_string);
            result.push(PluginManifestItem {
                client: "claude".to_string(),
                kind: "plugin".to_string(),
                name: name.clone(),
                version,
                source: None,
            });
        }
    }
    if let Some(entries) = marketplaces.as_object() {
        for (name, entry) in entries {
            let source = entry
                .get("source")
                .and_then(Value::as_object)
                .and_then(|source| {
                    let kind = source.get("source").and_then(Value::as_str)?;
                    let repo = source.get("repo").and_then(Value::as_str);
                    Some(
                        repo.filter(|repo| !is_machine_path(repo) && !is_sensitive_string(repo))
                            .map(|repo| format!("{}:{}", kind, repo))
                            .unwrap_or_else(|| kind.to_string()),
                    )
                });
            result.push(PluginManifestItem {
                client: "claude".to_string(),
                kind: "marketplace".to_string(),
                name: name.clone(),
                version: None,
                source,
            });
        }
    }
    Ok(result)
}

fn read_codex_plugins(path: &Path) -> Result<Vec<PluginManifestItem>, String> {
    let root = read_toml(path)?;
    let mut result = Vec::new();
    for (key, kind) in [("plugins", "plugin"), ("marketplaces", "marketplace")] {
        let Some(entries) = root.get(key).and_then(toml::Value::as_table) else {
            continue;
        };
        for (name, value) in entries {
            let version = value
                .get("version")
                .and_then(toml::Value::as_str)
                .map(str::to_string);
            result.push(PluginManifestItem {
                client: "codex".to_string(),
                kind: kind.to_string(),
                name: name.clone(),
                version,
                source: if kind == "marketplace" {
                    portable_marketplace_source(value)
                } else {
                    None
                },
            });
        }
    }
    Ok(result)
}

fn portable_marketplace_source(value: &toml::Value) -> Option<String> {
    let table = value.as_table()?;
    let kind = table.get("source").and_then(toml::Value::as_str);
    let location = ["repo", "url"]
        .iter()
        .find_map(|key| table.get(*key).and_then(toml::Value::as_str))
        .filter(|value| !is_sensitive_string(value) && !is_machine_path(value));
    match (kind, location) {
        (Some(kind), Some(location)) => Some(format!("{}:{}", kind, location)),
        (Some(kind), None) => Some(kind.to_string()),
        (None, Some(location)) => Some(location.to_string()),
        (None, None) => None,
    }
}

pub fn read_config_sync_manifest() -> Result<ConfigSyncManifest, String> {
    let home = home_dir()?;
    let claude_path = home.join(".claude.json");
    let codex_path = home.join(".codex").join("config.toml");
    let mut mcp_servers = claude_mcp_entries(&claude_path)?;
    mcp_servers.extend(codex_mcp_entries(&codex_path)?);
    mcp_servers.sort_by(|a, b| (&a.client, &a.name).cmp(&(&b.client, &b.name)));

    let mut plugins = read_claude_plugins(&home)?;
    plugins.extend(read_codex_plugins(&codex_path)?);
    plugins.sort_by(|a, b| (&a.client, &a.kind, &a.name).cmp(&(&b.client, &b.kind, &b.name)));
    Ok(ConfigSyncManifest {
        mcp_servers,
        plugins,
    })
}

fn raw_mcp_config(client: &str, name: &str, home: &Path) -> Result<Option<Value>, String> {
    match client {
        "claude" => Ok(read_json(&home.join(".claude.json"))?
            .get("mcpServers")
            .and_then(Value::as_object)
            .and_then(|servers| servers.get(name))
            .cloned()),
        "codex" => read_toml(&home.join(".codex").join("config.toml"))?
            .get("mcp_servers")
            .and_then(toml::Value::as_table)
            .and_then(|servers| servers.get(name))
            .map(json_from_toml)
            .transpose(),
        _ => Err(format!("不支持的 MCP 客户端: {}", client)),
    }
}

fn persist_backup(path: &Path, label: &str) -> Result<Option<PathBuf>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = dirs::config_dir()
        .ok_or_else(|| "无法定位应用配置目录".to_string())?
        .join("ai-session-viewer")
        .join("config-backups")
        .join(stamp.to_string());
    fs::create_dir_all(&dir).map_err(|e| format!("创建配置备份目录失败: {}", e))?;
    let backup = dir.join(label);
    fs::copy(path, &backup).map_err(|e| format!("备份配置失败: {}", e))?;
    Ok(Some(backup))
}

fn write_text(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建配置目录失败: {}", e))?;
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("config");
    let temp = path.with_extension(format!("{}.asv.tmp", extension));
    fs::write(&temp, content).map_err(|e| format!("写入临时配置失败: {}", e))?;
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(format!("替换配置失败: {}", error));
    }
    Ok(())
}

fn write_claude_mcp(path: &Path, name: &str, config: Value) -> Result<(), String> {
    let mut root = read_json(path)?;
    let root = root
        .as_object_mut()
        .ok_or_else(|| "Claude 配置根节点必须是对象".to_string())?;
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| "Claude mcpServers 必须是对象".to_string())?;
    servers.insert(name.to_string(), config);
    let mut content = serde_json::to_string_pretty(&root)
        .map_err(|e| format!("序列化 Claude 配置失败: {}", e))?;
    content.push('\n');
    write_text(path, &content)
}

fn write_codex_mcp(path: &Path, name: &str, config: Value) -> Result<(), String> {
    let content = if path.exists() {
        fs::read_to_string(path).map_err(|e| format!("读取 Codex 配置失败: {}", e))?
    } else {
        String::new()
    };
    let mut document = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("解析 Codex 配置失败: {}", e))?;
    if !document.as_table().contains_key("mcp_servers") {
        document["mcp_servers"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let servers = document["mcp_servers"]
        .as_table_like_mut()
        .ok_or_else(|| "Codex mcp_servers 必须是表".to_string())?;
    let value: toml::Value =
        serde_json::from_value(config).map_err(|e| format!("转换 MCP 配置失败: {}", e))?;
    let item = toml_edit::ser::to_document(&value)
        .map_err(|e| format!("生成 MCP TOML 失败: {}", e))?
        .into_item();
    servers.insert(name, item);
    write_text(path, &document.to_string())
}

fn restore_config(path: &Path, backup: Option<&Path>) -> Result<(), String> {
    if let Some(backup) = backup {
        fs::copy(backup, path)
            .map(|_| ())
            .map_err(|e| format!("恢复配置备份失败: {}", e))
    } else if path.exists() {
        fs::remove_file(path).map_err(|e| format!("清理新配置失败: {}", e))
    } else {
        Ok(())
    }
}

fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
}

pub fn apply_mcp_server(request: ApplyMcpRequest) -> Result<(), String> {
    let _guard = CONFIG_WRITE_LOCK
        .lock()
        .map_err(|_| "配置写入锁不可用".to_string())?;
    if !valid_name(&request.name) || !request.config.is_object() {
        return Err("MCP 名称或配置无效".to_string());
    }
    let (sanitized, _) = sanitize_config(&request.config);
    if sanitized != request.config {
        return Err("MCP 同步数据仍包含敏感字段".to_string());
    }

    let home = home_dir()?;
    let current = raw_mcp_config(&request.client, &request.name, &home)?;
    if config_hash(current.as_ref()) != request.expected_hash {
        return Err("目标 MCP 配置在预览后已变化，请刷新预览".to_string());
    }
    if current.is_some() && !request.overwrite {
        return Err(format!("目标已存在: {}", request.name));
    }

    let mut config = request.config.clone();
    if let Some(current) = &current {
        preserve_target_secrets(&mut config, current);
    }
    let (path, label) = match request.client.as_str() {
        "claude" => (home.join(".claude.json"), "claude.json"),
        "codex" => (home.join(".codex").join("config.toml"), "codex.toml"),
        other => return Err(format!("不支持的 MCP 客户端: {}", other)),
    };
    let backup = persist_backup(&path, label)?;
    let write_result = match request.client.as_str() {
        "claude" => write_claude_mcp(&path, &request.name, config),
        "codex" => write_codex_mcp(&path, &request.name, config),
        _ => unreachable!(),
    };
    if let Err(error) = write_result {
        restore_config(&path, backup.as_deref())?;
        return Err(error);
    }

    let verified = match raw_mcp_config(&request.client, &request.name, &home) {
        Ok(Some(value)) => sanitize_config(&value).0 == request.config,
        Ok(None) => false,
        Err(error) => {
            restore_config(&path, backup.as_deref())?;
            return Err(format!("写入后读取配置失败: {}", error));
        }
    };
    if !verified {
        restore_config(&path, backup.as_deref())?;
        return Err("MCP 配置写入后校验失败，已恢复备份".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_mcp_secrets_without_hiding_portable_fields() {
        let input = serde_json::json!({
            "command": "npx",
            "args": ["server"],
            "env": {"API_KEY": "secret", "MODE": "fast"},
            "headers": {"Authorization": "Bearer secret", "X-Mode": "fast"}
        });
        let (clean, redacted) = sanitize_config(&input);
        assert_eq!(clean["command"], "npx");
        assert_eq!(clean["env"]["MODE"], "fast");
        assert!(clean["env"].get("API_KEY").is_none());
        assert!(clean["headers"].get("Authorization").is_none());
        assert_eq!(redacted, vec!["env.API_KEY", "headers.Authorization"]);
        let other_secret = serde_json::json!({
            "command": "npx",
            "args": ["server"],
            "env": {"API_KEY": "different", "MODE": "fast"},
            "headers": {"Authorization": "Bearer different", "X-Mode": "fast"}
        });
        assert_eq!(config_hash(Some(&input)), config_hash(Some(&other_secret)));

        let machine_specific = serde_json::json!({"command": "C:\\tools\\server.exe"});
        assert!(sanitize_config(&machine_specific)
            .0
            .get("command")
            .is_none());
    }
}
