# Sync 模块重构计划

## 背景

当前 `sync` 模块需要适配 ESP32-C3 的资源限制：设备可用内存约 400KB，同时同步目标应写入 SD 卡的 `vault/notes` 目录，而不是直接写入 `vault` 根目录。

现有代码里有几个需要修复的问题：

1. 旧 manifest、新 manifest、seen keys 都以 `HashMap` / `HashSet` 全量驻留内存。
2. 写 `.rr_sync_status` 时一次性构造完整 `String`，会造成额外内存峰值。
3. 读 `.rr_sync_status` 时一次性 `read_to_string`，会造成额外内存峰值。
4. `seen_continuation_tokens` 使用无界 `HashSet`，不适合嵌入式环境。
5. 同步内容当前路径倾向于 `/sdcard/vault`，但项目当前约定同步内容应放在 `/sdcard/vault/notes`。
6. stale 删除范围过宽，应该只删除 sync 管理的 notes 内容目录。
7. Range 下载 chunk 较小，下载效率偏低。
8. 小文件 direct download 被禁用。
9. `.rrpart.meta` 每个 chunk 重复写入，增加 SD 卡写入和耗时。
10. 部分路径编码和签名候选逻辑存在不必要的小分配。

## 目标

1. 将同步内容根目录改为 `/sdcard/vault/notes`。
2. 保留同步状态文件在 `/sdcard/vault/.rr_sync_status`。
3. 将 manifest 处理改为非全量内存模式。
4. 保持断点续传和 stale 删除语义安全。
5. 减少堆分配，降低 heap 峰值和碎片化风险。
6. 在不明显增加 RAM 的前提下提高下载效率。

## 目录约定

最终 SD 卡结构应为：

- `/sdcard/vault/.rr_reader.conf`
- `/sdcard/vault/.rr_sync_status`
- `/sdcard/vault/.rr_sync_status.tmp`
- `/sdcard/vault/.rr_sync_status.bak`
- `/sdcard/vault/.rr_sync_status.entries.tmp`
- `/sdcard/vault/notes/...`

从电脑上看 SD 卡时，对应为：

- `vault/notes/...`

代码中统一使用小写 `vault` 和 `notes`。

## 具体任务

### 任务 1：修正同步内容根目录

修改 `sync` 模块里的目录常量：

- 保留 `VAULT_ROOT = "/sdcard/vault"`
- 新增 `SYNC_CONTENT_ROOT = "/sdcard/vault/notes"`
- 保留 `SYNC_STATUS_PATH = "/sdcard/vault/.rr_sync_status"`

要求：

1. 同步开始前确保 `SYNC_CONTENT_ROOT` 存在。
2. `key_to_local_path` 将远端对象 key 映射到 `SYNC_CONTENT_ROOT` 下。
3. stale 删除只能删除 `SYNC_CONTENT_ROOT` 下的文件。
4. 不允许 stale 删除 `/sdcard/vault` 根目录下的配置、状态文件或手动文件。

### 任务 2：重构 manifest 为非全量内存模式

移除主流程中的全量结构：

- `previous_manifest: HashMap<String, SyncManifestEntry>`
- `new_manifest: HashMap<String, SyncManifestEntry>`
- `seen_keys: HashSet<String>`

改为：

1. 查询旧 manifest 时，按需流式扫描 `.rr_sync_status`。
2. 新 manifest entries 写入 `/sdcard/vault/.rr_sync_status.entries.tmp`。
3. 每处理一个远端对象，就 append 一条 manifest entry 到 entries 临时文件。
4. 最后将 header + entries tmp + footer 流式合成新的 `.rr_sync_status`。

需要提供或保留的接口：

- `SyncManifestWriter::new()`
- `SyncManifestWriter::append_entry(key, entry)`
- `SyncManifestWriter::entries_path()`
- `SyncManifestWriter::finalize(config, downloaded, skipped, deleted)`
- `find_sync_manifest_entry(key)`
- `delete_stale_manifest_files(new_entries_path)`

### 任务 3：修正 skip 与 stale 删除语义

远端对象出现但本地跳过时，不能误删旧本地文件。

处理策略：

1. 如果 key 是目录 marker、内部 marker、过长路径、不合法路径：
   - 计入 skipped。
   - 如果旧 manifest 有对应 entry，则 append 旧 entry 到新 entries 文件。
2. 如果下载失败：
   - 计入 skipped。
   - 如果旧 manifest 有对应 entry，则 append 旧 entry 到新 entries 文件。
3. 如果 unchanged：
   - append 当前 entry 到新 entries 文件。
4. 如果下载成功：
   - append 当前 entry 到新 entries 文件。
5. 最终 stale 删除只删除旧 manifest 中存在、但新 entries 文件中不存在的文件。

### 任务 4：移除分页 token 无界 HashSet

删除：

- `seen_continuation_tokens: HashSet<String>`

改为：

1. 只保存上一页 token。
2. 如果下一页 token 和上一页 token 相同，则报错。
3. 继续保留 `LIST_MAX_PAGES` 作为兜底保护。

### 任务 5：优化下载常量

调整：

- `DOWNLOAD_CHUNK_BYTES`：从 `16 * 1024` 改为 `32 * 1024`
- `DOWNLOAD_DIRECT_MAX_BYTES`：从 `0` 改为 `8 * 1024`

理由：

1. 读取 buffer 仍为 1024 字节，不会显著增加 RAM。
2. 小文件可以减少 Range 请求和 TLS 握手次数。
3. 大文件仍使用 Range 断点续传。

### 任务 6：减少 `.rrpart.meta` 重复写入

当前每个 Range chunk 都写一次 meta。

改为：

1. `ensure_partial_matches` 继续负责校验旧 partial 是否匹配当前远端对象。
2. 开始 Range 下载前，如果 meta 不存在，写一次。
3. 每个 chunk 成功后不再重复写 meta。

### 任务 7：优化路径编码分配

`encode_path_segments` 不再使用：

- `collect::<Vec<_>>()`
- `join("/")`

改为直接写入一个 `String`。

这项已经完成，但需要最终检查编译结果。

### 任务 8：优化 signing candidates 分配

当前 `signing_candidates` 每次创建 `Vec`，但实际只有一个候选：

- service: `oss`
- region: `config.region`

改为单个 `SigningCandidate` 或低分配写法。

要求：

1. 不改变签名语义。
2. 保留清晰错误信息。
3. 避免每个请求创建不必要的 Vec。

### 任务 9：编译和诊断

完成代码修改后执行：

1. `cargo check` 或项目可用的检查命令。
2. 如果交叉编译环境不完整，则至少执行 diagnostics。
3. 修复 1-2 轮明确的编译错误。

重点检查：

- unused import
- 函数签名不匹配
- moved value
- borrow checker 问题
- stale 删除路径保护
- manifest writer finalize 后临时文件清理

## 实施顺序

建议按以下顺序执行：

1. 修正目录常量，增加 `SYNC_CONTENT_ROOT`。
2. 修正 `path_codec.rs` 使用 `SYNC_CONTENT_ROOT`。
3. 修正 `manifest.rs` stale 删除范围为 `SYNC_CONTENT_ROOT`。
4. 修改 `mod.rs` 接入 `SyncManifestWriter`。
5. 移除 manifest 相关 `HashMap` / `HashSet`。
6. 移除 `seen_continuation_tokens`。
7. 调整下载常量。
8. 优化 `.rrpart.meta` 写入。
9. 优化 signing candidates。
10. 运行检查并修复编译问题。

## 风险与取舍

### O(N²) manifest 查询风险

非全量内存模式下，如果每个远端 key 都扫描一次旧 manifest，速度会变慢。

这是为了优先保证 ESP32-C3 上的内存安全。后续如果文件数量很多，可以再做磁盘索引或排序归并。

### entries tmp 文件查找风险

stale 删除时，旧 manifest 每个 key 会扫描新 entries tmp 文件，复杂度也是 O(N²)。

这是低内存实现。后续可优化为：

1. entries tmp 排序后归并。
2. 构建小型磁盘索引。
3. 分桶临时文件。

### skip 保留旧 entry 的风险

跳过远端对象时，如果旧 manifest 有 entry，会保留旧 entry，避免误删。

这可能导致某些历史记录继续留在 manifest 里，但比误删用户文件安全。

## 完成标准

1. 同步内容写入 `/sdcard/vault/notes`。
2. `.rr_sync_status` 仍在 `/sdcard/vault` 根目录。
3. 主同步流程不再全量持有旧 manifest、新 manifest、seen keys。
4. stale 删除只作用于 `/sdcard/vault/notes`。
5. 小文件 direct download 启用。
6. Range chunk 调整为 32KB。
7. `.rrpart.meta` 不再每个 chunk 重复写。
8. 编译检查通过或只剩明确的外部环境问题。
