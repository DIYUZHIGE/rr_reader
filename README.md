# rr_reader — Obsidian Vault 墨水屏阅读器

通过 Obsidian remotely-save 插件的配置 URL，从 S3 兼容存储拉取 vault（markdown 文件），在 阅星曈 X4 墨水屏上离线阅读。

## 硬件参数（来自 crosspoint-reader 实际测量）

| 参数 | 值 |
|------|-----|
| MCU | ESP32-C3 (RISC-V, 160MHz) |
| RAM | ~380KB 可用，**无 PSRAM** |
| Flash | 16MB |
| 屏幕 | 4.26" 墨水屏, SSD1677 控制器, 800×480, 黑白 |
| 输入 | 7 个物理按键 (4 前 + 2 侧 + 电源), ADC 电阻分压 + 数字引脚 |
| 存储 | MicroSD (SPI, 共享总线) |
| 无线 | WiFi 4 (802.11b/g/n) |

### GPIO 引脚分配（已验证）

```
显示 (SPI2):
  SCLK  GPIO8    CS    GPIO21
  MOSI  GPIO10   DC    GPIO4
  MISO  GPIO7    RST   GPIO5
                  BUSY  GPIO6 (输入, active HIGH)

SD 卡 (共享 SPI2):
  CS    GPIO12

按键:
  前按钮组  GPIO1 (ADC1_CH1, 11dB衰减)
  侧按钮组  GPIO2 (ADC1_CH2, 11dB衰减)
  电源键    GPIO3 (数字输入, 上拉, active LOW)

USB 检测:
  RXD   GPIO20 (UART0)
```

### 按键 ADC 阈值（crosspoint 实测值）

| 按键 | GPIO | ADC 范围 |
|------|------|----------|
| Back (前左) | GPIO1 | 3100–3800 |
| Confirm (前2) | GPIO1 | 2090–3100 |
| Left (前3) | GPIO1 | 750–2090 |
| Right (前右) | GPIO1 | 0–750 |
| Up (侧上) | GPIO2 | 1120–3800 |
| Down (侧下) | GPIO2 | 0–1120 |
| 无按下 | — | ≥3800 |

## 架构

```
┌─────────────────────────────────────────────────────────┐
│                      rr_reader                           │
│                                                          │
│  ┌──────────┐  ┌──────────┐  ┌──────────────────────┐  │
│  │ Obsidian │  │   S3     │  │   Markdown            │  │
│  │ URL 解析 │  │  Client  │  │   Renderer            │  │
│  │          │  │          │  │                       │  │
│  │ URL解码  │  │ ListObj  │  │ 解析→分页→字体→       │  │
│  │ 参数提取 │  │ GetObj   │  │ framebuffer→刷新      │  │
│  └────┬─────┘  └────┬─────┘  └───────────┬───────────┘  │
│       │              │                    │              │
│  ┌────┴──────────────┴────────────────────┴──────────┐  │
│  │                     Core                            │  │
│  │  ┌─────────┐  ┌───────────┐  ┌─────────────────┐  │  │
│  │  │ WiFi    │  │ SD Cache  │  │  Input /        │  │  │
│  │  │ Manager │  │ Manager   │  │  Button Mgr     │  │  │
│  │  └─────────┘  └───────────┘  └─────────────────┘  │  │
│  │  ┌─────────┐  ┌───────────┐  ┌─────────────────┐  │  │
│  │  │ Power   │  │  Display  │  │  Activity       │  │  │
│  │  │ Manager │  │  Driver   │  │  Manager        │  │  │
│  │  └─────────┘  └───────────┘  └─────────────────┘  │  │
│  └────────────────────────────────────────────────────┘  │
│                                                          │
│  ESP32-C3 + SSD1677 E-Ink + SD Card (SPI shared bus)    │
└─────────────────────────────────────────────────────────┘
```

## 模块拆分

### 0. 硬件抽象层 (HAL)

参考 crosspoint 的 HalGPIO、HalDisplay、HalStorage 模式。

- **外设初始化**: SPI2 总线 (40MHz), GPIO 配置（含 deep sleep hold 释放）
- **显示驱动改进**: SPI 从 10MHz 提升到 40MHz, 实现 partial/fast refresh
- **SD 卡驱动**: CS=GPIO12, 40MHz, 共享 SPI 总线
- **按键驱动**: ADC 轮询 + 5ms 软件去抖，逻辑按键映射（参考 MappedInputManager）
- **电源管理**: 深度睡眠 + GPIO3 唤醒 (ESP_GPIO_WAKEUP_GPIO_LOW)

### 1. 显示子系统 (`display`)

**当前状态**: 已切到共享 SPI2 40MHz，并实现 Full / Half / Fast 三种刷新模式；仍需硬件实测刷新质量和残影策略。

**改进计划**（参考 EInkDisplay.cpp）:

- [x] SPI 频率提升到 40MHz (SSD1677 可接受)
- [x] 实现 FAST_REFRESH (差分刷新):
  - BW RAM (0x24) 写新帧，RED RAM (0x26) 保留旧帧
  - Update Ctrl1 = 0x00 (normal mode, 差分比较)
  - Update Ctrl2 = 0x1C (LUT_LOAD + MODE_SELECT + DISPLAY_START)
  - 耗时 ~400-600ms vs 全刷 ~1600ms
- [x] 实现 HALF_REFRESH (平衡刷新):
  - 写温度寄存器 0x5A
  - Update Ctrl2 = 0xD4 (跳过温度加载)
- [ ] FULL_REFRESH: 保留当前实现，用于开机、唤醒、每 N 次 fast 后清除残影
- [x] 刷新策略: 翻页用 FAST，菜单切换用 HALF，启动/唤醒用 FULL，每 20 次 FAST 插入一次 FULL
- [x] RED RAM 同步: single buffer 模式下，每次刷新后将当前帧回写到 RED RAM 作为下次差分的基准
- [ ] 硬件实测: 确认 BUSY 极性、Fast/Half 残影、40MHz SPI 稳定性

### 2. 输入子系统 (`input`)

**参考**: InputManager (ADC 读取+去抖), HalGPIO (封装), MappedInputManager (逻辑映射)

- [x] ADC 驱动: GPIO1/GPIO2 的 ADC 读取 (11dB 衰减)
- [x] 按键解码: 根据 ADC 值查表确定按下的按键
- [x] 软件去抖: 约 20ms 稳定窗口（主循环 10ms，2 tick）
- [x] 按下/释放事件检测 (wasPressed/wasReleased)
- [x] 长按检测 (hold duration)
- [x] 逻辑按键映射:
  - PageBack/PageForward (侧键，可交换)
  - Confirm/Back/Left/Right (前按键，可重映射)
- [ ] 按键组合检测 (如 Power+Down 截图)
- [ ] 硬件实测: 校准 ADC 阈值和按键去抖时间

### 3. 存储子系统 (`storage`)

**参考**: SDCardManager (底层), HalStorage (互斥封装), FsHelpers

- [x] SD 卡初始化: SPI CS=GPIO12, FAT 文件系统
- [x] 共享 SPI 总线管理: 显示和 SD 卡共用 SPI2，通过 CS 引脚切换
- [ ] 缓存目录结构:
  ```
  /vault/
  ├── config.json          # S3 配置和同步状态
  ├── file_index.json      # 远程文件索引 (path→etag, mtime)
  ├── sync_state.json      # 上次同步时间、待同步列表
  └── notes/               # 下载的 markdown 文件
      ├── daily/
      ├── projects/
      └── ...
  ```
- [ ] 文件操作: 原子写入（先写临时文件再重命名），目录递归创建
- [ ] 读取进度持久化: `/vault/reading_progress.json`

### 4. WiFi 管理 (`wifi`)

**参考**: WifiSelectionActivity, WifiCredentialStore

- [ ] WiFi STA 模式: 扫描 → 选择 → 连接
- [ ] 凭据存储: `/vault/wifi.json` (JSON 格式)
  - SSID + 密码 (MAC 地址 XOR 混淆 + base64)
  - lastConnectedSsid 自动重连
- [ ] 连接超时: 15 秒
- [ ] WiFi 状态监控: 掉线检测，自动重连
- [ ] `WiFi.setSleep(false)` 在同步期间禁用省电
- [ ] AP 模式 (可选): 用于初始配置，SSID="rr-reader-setup"

### 5. Obsidian URL 解析 (`obsidian_config`)

- [ ] URL decode: `%7B` → `{`, `%22` → `"`, 等
- [ ] JSON 解析提取 S3 配置:
  - `s3Endpoint`: S3 兼容存储地址
  - `s3Region`: 区域
  - `s3AccessKeyID`: Access Key
  - `s3SecretAccessKey`: Secret Key
  - `s3BucketName`: Bucket 名称
  - `remotePrefix`: 远程路径前缀（通常是 vault 名）
- [ ] 配置输入方式:
  - 二维码扫描 (首选，URL 转二维码后摄像头/手机扫描)
  - 手动输入 (通过按键选择字符，备选)
  - 配置文件放入 SD 卡 (最简实现)

### 6. S3 客户端 (`s3_client`)

**关键挑战**: 在 ESP32-C3 上实现 AWS Signature V4

- [ ] AWS SigV4 签名:
  - HMAC-SHA256 (使用 mbedtls，esp-idf-svc 内置)
  - SHA256 哈希
  - Canonical Request 构造
  - String to Sign 计算
  - Authorization Header 生成
- [ ] HTTPS 请求 (使用 esp-idf-svc 的 HTTP client + TLS)
- [ ] ListObjectsV2: 列出 bucket 中的文件
- [ ] GetObject: 下载单个文件
- [ ] 增量同步策略:
  - 首次: 全量下载
  - 后续: 对比 ETag 或 LastModified，只下载变更文件
  - 本地删除检测: 远程无变更但本地有文件的，保留本地
- [ ] 分页处理: ListObjects 最多返回 1000 个对象，需要处理分页
- [ ] 错误处理: 网络超时、403 鉴权失败、404 文件不存在

### 7. Markdown 渲染 (`md_render`)

**关键挑战**: 在 380KB RAM 下渲染中文 markdown，支持 CJK 字体

- [ ] Markdown 解析器 (轻量级，流式处理):
  - 标题 (H1-H6)
  - 段落
  - 加粗/斜体 (`**bold**`, `*italic*`)
  - 无序/有序列表
  - 代码块 (缩进显示，等宽字体)
  - 链接 `[text](url)` — 显示 text，忽略 url
  - 图片 `![](url)` — 显示占位符 `[Image]`
  - 水平线 `---`
  - 引用 `> quote`
  - Obsidian 特有语法:
    - Wiki 链接 `[[note]]` — 显示为链接，可选跳转
    - 嵌入 `![[note]]` — 暂显示为引用，后续支持展开
    - Callout `> [!note]` — 特殊格式块
    - 标签 `#tag` — 显示但不交互
- [ ] 分页计算: 根据字体大小、屏幕尺寸、行距计算分页
- [ ] 中文排版: 标点压缩、行首行尾禁则
- [ ] 流式渲染: 文件分块读取，逐段解析渲染，避免全文加载到 RAM

### 8. 字体系统 (`font`)

**参考**: crosspoint 的 EpdFont 格式和 GfxRenderer

- [ ] 字体格式设计: 简化版 EpdFont
  - 字形位图数据 (1-bit 或 2-bit，DEFLATE 压缩)
  - Unicode interval 索引表
  - Kerning 表 (可选)
- [ ] 中文字体: 至少包含 CJK Unified Ideographs (0x4E00–0x9FFF, 20992 字)
  - 字体大小: 16px (阅读), 24px (标题)
  - 存储: Flash 中 DEFLATE 压缩存储 (~200-300KB per size)
  - 热缓存: 解压当前页需要的字形到 RAM (~10-20KB)
- [ ] 西文字体: Noto Sans 12/14/16px
- [ ] 等宽字体: 用于代码块 (12px)
- [ ] 字体预加载策略 (参考 prewarm): 扫描页面文本 → 解压用到的字 → 渲染

### 9. 文件浏览器 (`file_browser`)

- [x] 基于本地缓存的 Markdown 文件列表
- [ ] 基于本地缓存的目录树
- [x] 文件列表（按名称排序）
- [ ] 显示同步状态: 已同步 / 有更新 / 仅本地
- [ ] 目录层级导航（退格键返回上级）

### 10. Activity 框架 (`activity`)

**参考**: crosspoint 的 ActivityManager + Activity 生命周期

- [x] 最小 Activity 状态机: FileBrowser / Reader
- [ ] Activity 生命周期: `onEnter()` → `loop()` → `render()` → `onExit()`
- [ ] ActivityManager: 当前 Activity 管理，replace/push/pop 导航
- [ ] 渲染任务: 独立 FreeRTOS 任务处理渲染（避免阻塞主循环）
- [ ] Activity 类型:
  - BootActivity: 启动画面
  - HomeActivity: 笔记列表（文件浏览）
  - ReaderActivity: 阅读模式
  - SettingsActivity: 设置
  - WifiSetupActivity: WiFi 配置
  - SyncActivity: 同步进度显示

### 11. 同步策略 (`sync`)

- [ ] 手动同步: 用户在设置页面触发
- [ ] 自动同步: 定时唤醒 (每 N 小时，用户可配置)
- [ ] 同步流程:
  1. 连接 WiFi
  2. ListObjects 获取远程文件列表
  3. 对比本地 file_index.json
  4. 下载新增/变更的文件
  5. 删除本地多余文件的标记（不实际删除）
  6. 更新 file_index.json
  7. 断开 WiFi
- [ ] 文件保留策略:
  - 默认保留所有已下载文件（离线可用）
  - 可选 "仅保留最近 N 天笔记"
  - 手动清理缓存

### 12. 电源管理 (`power`)

**参考**: crosspoint 的深度睡眠和唤醒处理

- [ ] 深度睡眠模式:
  - 唤醒源: GPIO3 (电源键, 低电平唤醒)
  - 定时唤醒: ESP32-C3 内置 RTC 定时器 (用于定时同步)
- [ ] 唤醒原因处理:
  - 电源键唤醒 → 正常启动
  - USB 上电 → 直接重新进入深度睡眠（仅充电不开机）
  - 定时器唤醒 → 静默同步后重新睡眠
- [ ] 自动休眠: 空闲 N 分钟后进入深度睡眠
- [ ] 电源键长按检测: 确认是用户主动开机而非误触
- [ ] 电池监控 (可选, 需要 ADC 引脚)

## 实施阶段

### Phase 0: 硬件基础（已完成）
- [x] ESP32-C3 启动 + esp-idf-hal 外设初始化
- [x] SSD1677 墨水屏 Full Refresh 驱动 (SPI 10MHz)
- [x] 基础 app loop 框架

### Phase 0.5: 硬件完善（对齐 crosspoint）
- [x] 显示 SPI 提升到 40MHz
- [x] 实现 FAST_REFRESH (差分刷新) 和 HALF_REFRESH
- [x] SD 卡驱动 (SPI CS=GPIO12, FAT)
- [x] GPIO/按键驱动 (ADC 轮询 + 去抖)
- [x] 逻辑按键映射
- [x] 深度睡眠 + 唤醒

### Phase 1: 显示和交互
- [x] 中文字体渲染 (内置压缩点阵字体)
- [ ] Markdown 解析 + 分页引擎
- [x] 基础 UI: 文字绘制、文件列表
- [x] 最小 Activity 框架
- [x] 文件浏览器基础功能
- [ ] 阅读模式: 翻页、目录跳转

### Phase 2: 网络和同步
- [ ] WiFi 扫描/连接/凭据持久化
- [ ] Obsidian URL 解析 (支持配置文件输入)
- [ ] AWS SigV4 签名实现
- [ ] S3 Client: ListObjects + GetObject
- [ ] 基础同步流程

### Phase 3: 阅读体验
- [ ] Obsidian Wiki 链接支持
- [ ] 阅读进度保存和恢复
- [ ] 代码块语法高亮（简化版）
- [ ] Callout 块渲染
- [ ] 增量同步
- [ ] 配置向导（WiFi + S3 配置）

### Phase 4: 打磨
- [ ] 定时唤醒 + 自动同步
- [ ] 电池电量显示
- [ ] 错误恢复和用户提示
- [ ] 性能优化（内存、刷新速度）
- [ ] 字体大小/行距设置

## 当前代码结构

```
rr_reader/
├── Cargo.toml
├── build.rs
├── sdkconfig.defaults
├── partitions.csv
├── rust-toolchain.toml
└── src/
    ├── main.rs
    ├── app.rs           # 最小 Activity 状态机: FileBrowser / Reader
    ├── display.rs       # SSD1677 驱动: 40MHz + Full/Half/Fast refresh
    ├── font.rs          # 压缩点阵字体加载和渲染
    ├── hardware.rs      # 硬件封装: 输入 + 存储
    ├── input.rs         # ADC 按键、去抖、逻辑映射
    ├── storage.rs       # SD/FAT 挂载和 Markdown 文件读取
    └── power.rs         # 深度睡眠和唤醒
```

## 关键技术决策

### 为什么用 Rust 而不是继续用 crosspoint 的 C++?

1. **内存安全**: Rust 的所有权系统在 ESP32-C3 这种无 MMU、无 PSRAM 的平台上避免了 use-after-free 和内存泄漏
2. **类型系统**: ADT (enum) 适合状态机和错误处理，不需要跨层传递 errno
3. **依赖管理**: Cargo 比 PlatformIO 的 lib_deps 更可靠
4. **安全关键**: 处理用户的 S3 凭据，Rust 减少缓冲区溢出风险

### 风险评估

| 风险 | 影响 | 缓解措施 |
|------|------|----------|
| ESP32-C3 的 Rust 生态不成熟 | 中 | esp-idf-hal/svc 已有官方支持，踩坑可回退 C |
| CJK 字体 Flash 占用大 | 中 | DEFLATE 压缩 + 按需解压到 RAM |
| AWS SigV4 实现复杂 | 中 | 可用 mbedtls 的 HMAC-SHA256，核心逻辑 <500 行 |
| 中文字体在 800x480 可读性 | 低 | 16px 字体在 4.26" 屏上相当于约 300PPI 的文字 |
| S3 同步耗电 | 低 | 仅在用户触发或定时唤醒时同步，平时深度睡眠 |

## 与 crosspoint-reader 的关系

crosspoint-reader 是本项目的关键参考实现，已验证了硬件参数和工作模式：

| 方面 | crosspoint-reader | rr_reader |
|------|-------------------|-----------|
| 语言 | C++20 (Arduino) | Rust (ESP-IDF) |
| 框架 | PlatformIO | Cargo + embuild |
| 关注点 | EPUB 阅读 | Markdown/Obsidian vault |
| 内容来源 | SD 卡 / WiFi 上传 | S3 远程同步 |
| 字体 | Noto Serif/Sans 拉丁字符集 | 需增加 CJK 中文字体 |
| 刷新模式 | Full + Half + Fast + Gray | 当前只有 Full，计划加上 Fast |
| SPI 速度 | 40MHz | 当前 10MHz，计划提升到 40MHz |
