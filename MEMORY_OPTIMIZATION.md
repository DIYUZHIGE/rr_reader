# 内存优化措施总结

## 已实施的优化

### 1. ESP-IDF 系统配置优化 (sdkconfig.defaults)
- **主任务栈**: 32KB → 8KB (节省 24KB)
- **事件任务栈**: 默认 → 2KB
- **定时器任务栈**: 默认 → 2KB
- **pthread栈**: 默认 → 2KB
- **日志级别**: INFO → WARN (减少日志缓冲区)
- **FATFS长文件名**: 堆分配 → 栈分配 (节省堆碎片)
- **FATFS最大文件名**: 255 → 127 字节
- **WiFi缓冲区优化**:
  - 静态RX缓冲: 10 → 4
  - 动态RX缓冲: 32 → 8
  - 动态TX缓冲: 32 → 8
  - 禁用AMPDU (TX/RX)
- **LWIP优化**:
  - 最大socket: 10 → 4
  - TCPIP接收邮箱: 32 → 8
  - TCP接收邮箱: 6 → 4
  - UDP接收邮箱: 6 → 4
- **FreeRTOS优化**:
  - 空闲任务栈: 1536 → 768 字节
  - ISR栈: 1536 → 1024 字节
- **编译器优化**:
  - 优化目标: 速度 → 大小 (opt-level = "z")
  - 禁用断言检查

**预计节省**: ~40-50KB RAM

### 2. Rust编译优化 (Cargo.toml)
- **LTO**: 启用 "fat" (跨crate内联)
- **代码生成单元**: 16 → 1 (更好的优化)
- **符号剥离**: 启用
- **panic策略**: unwind → abort (节省展开表)
- **溢出检查**: 禁用 (release模式)
- **依赖优化**:
  - anyhow: 禁用默认特性
  - flate2: 使用rust_backend而非C库

**预计节省**: ~10-15KB Flash, ~2-5KB RAM

### 3. 字形缓存优化 (glyph_cache.rs)
- **缓存容量**: 64 → 32 个字形
- **清理策略**: 增强 - 同时清理scratch缓冲区并shrink_to_fit
- **内存回收**: 在加载新文件前主动清理

**预计节省**: ~8-16KB RAM (取决于字形大小)

### 4. 堆分配优化
- **WiFi配置解析**: Vec → 固定大小数组 [Option<String>; 2]
- **文本截断**: String::repeat → 栈上固定缓冲区
- **字符串预分配**: String::new() → String::with_capacity(64)

**预计节省**: ~1-2KB RAM (减少堆碎片)

### 5. 滑动窗口页缓存 (ReaderCache 重构)
- **策略**: 解析后保留 `Vec<RenderBlock>`(~100KB)，只缓存当前页 +/-2 页(共5页，~15KB)
- **窗口滑动**: 翻页超出窗口时，从保留的 blocks 重新分页，无需重新读取 SD 卡或重新解析 Markdown
- **内存生命周期**:
  - 解析阶段峰值: markdown 原文 + 预处理 + blocks + 全量 pages(~450KB，短暂存在)
  - 稳态: 仅 blocks + 5页窗口(~115KB)
- **对比旧方案**: 旧方案稳态保留全量 pages(~150KB)，峰值 ~650KB
- **API**: `ensure_window(page_index)` 滑动窗口，`get_page(page_index)` 不可变查找

**预计节省**: ~100-150KB RAM(稳态)，峰值降低 ~200KB

## 总计预计节省
- **RAM**: 150-225KB（含滑动窗口优化）
- **Flash**: 10-15KB

## 进一步优化建议

### 高优先级
1. **Framebuffer优化** (当前: 48KB)
   - 考虑使用外部PSRAM (如果硬件支持)
   - 或使用分块渲染 (tile-based rendering)

2. **解析阶段峰值优化**
   - 当前解析时同时存在 markdown 原文 + 预处理 + blocks + 全量 pages
   - 可改为边解析边分页边丢弃，避免同时持有全量 pages

3. **字符串优化**
   - 更多使用 &str 而非 String
   - 使用 SmallString/ArrayString 替代短字符串
   - 文件路径使用固定大小缓冲区

### 中优先级
4. **WiFi栈优化**
   - 连接后释放不必要的WiFi资源
   - 考虑按需初始化WiFi

5. **日志系统**
   - 生产版本完全禁用日志 (LOG_MAXIMUM_LEVEL_NONE)
   - 或使用条件编译

6. **依赖精简**
   - 审查pulldown-cmark是否可以用更轻量的解析器替代
   - 考虑自定义简化的markdown解析器

### 低优先级
7. **代码优化**
   - 使用 #[inline] 标记热路径函数
   - 减少泛型单态化
   - 使用 const fn 进行编译时计算

## 内存监控

编译后使用以下命令检查内存使用：
```bash
cargo size --release -- -A
cargo bloat --release
```

运行时监控：
```rust
unsafe {
    let free_heap = esp_idf_hal::sys::esp_get_free_heap_size();
    let min_free = esp_idf_hal::sys::esp_get_minimum_free_heap_size();
    info!("Free heap: {} bytes, Min: {} bytes", free_heap, min_free);
}
```

## 注意事项

1. **栈溢出风险**: 减小栈大小后需要测试所有功能，特别是：
   - WiFi连接
   - 文件系统操作
   - Markdown解析
   - 图像解码

2. **性能权衡**: 
   - 优化大小可能降低性能
   - 减小缓存会增加解压次数

3. **稳定性测试**:
   - 长时间运行测试
   - 大文件处理测试
   - 内存泄漏检测
