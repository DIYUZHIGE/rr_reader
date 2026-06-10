# rr_reader
---
> 我本人对ESP32几乎一无所知，这个项目的所有代码由DeepSeek完成。
---

在**阅星瞳 X4** 墨水屏设备上阅读 Obsidian vault 的固件。

- 将 vault 放到 SD 卡，设备启动后即可浏览和阅读所有 Markdown 文件
- 支持公式渲染、图片显示、WiFi 连接、S3 同步
- 基于 ESP32-C3 + ESP-IDF + Rust 构建

## 功能概览

### 文件浏览

- 递归扫描 `/sdcard/vault` 下的 `.md` / `.markdown` 文件
- 目录层级浏览：进入子目录、返回上级
- 显示文件名（不含扩展名）、当前选中项、目录内文件总数
- 支持通过 `/sdcard/vault/.rr_reader.conf` 配置默认浏览根目录

### Markdown 阅读

- 使用 `pulldown-cmark` 解析 Markdown，按屏幕尺寸自动分页
- 支持标题（1–6 级）、段落、无序/有序列表、任务列表、引用块、代码块、分割线、表格
- **Obsidian wiki 链接**：识别 `[[target|alias]]`，支持链接选中跳转和阅读历史回溯
- 边距缩进：列表、引用块、代码块均有独立缩进层级
- 页面缓存到 SD 卡，二次打开无需重新解析

### 公式渲染

- 行内公式 `$...$` 和块公式 `$$...$$`
- 支持的 LaTeX 结构：上标 `^`、下标 `_`、分数 `\frac{}{}`、根号 `\sqrt{}`、`\text{}`
- ~50 个常用符号映射（希腊字母、运算符、关系符、箭头等）
- 双字体系统：14px math 字体 + 12px script 字体，支持嵌套表达式
- 递归布局引擎：像素级精确的上下标偏移、分数线绘制、根号笔画

### 图片显示

- Markdown 图片 `![](path)` 和 Obsidian 嵌入 `![[path]]`
- JPEG 硬件加速解码（ESP-IDF tjpgd），自动缩放适配屏幕
- Floyd-Steinberg 抖动算法转 1-bit 单色
- 解码后的位图缓存到 SD 卡（`/sdcard/vault/.rr_cache/`），二次打开即时显示
- 非 JPEG 格式显示占位框（含文件名）
- 支持 Obsidian 尺寸提示（`|widthxheight`）

### WiFi 与 S3 同步

- WiFi STA 模式：扫描、密码连接、凭据持久化
- 支持 WPA2 和开放网络
- **S3 同步引擎**：兼容阿里云 OSS 及 S3 协议
  - OSS V4 签名（`OSS4-HMAC-SHA256`）
  - 分页列出远程对象，流式 XML 解析
  - 小文件直接下载，大文件分块续传（支持断点续传）
  - 清单驱动的增量同步（跳过未变更文件）
  - 自动清理远程已删除的本地文件
- 设置界面提供：手动同步、删除本地并重新同步、缓存所有图片
- 进入阅读模式自动暂停 WiFi，退出后恢复

### 电源管理

- 30 分钟无操作自动提示睡眠
- 长按电源键 2 秒进入深度睡眠
- 电源键唤醒（需按住确认）

### 显示刷新

三种刷新模式自动调度：

| 模式 | 耗时 | 场景 |
|------|------|------|
| Full | ~1600ms | 开机、唤醒 |
| Half | ~900ms | 菜单切换 |
| Fast | ~400ms | 翻页 |

每 50 次 Fast 刷新自动插入一次 Half 清理残影。

## 硬件

目标设备：**阅星瞳 X4**

| 部件 | 参数 |
|------|------|
| MCU | ESP32-C3 |
| RAM | 约 380KB 可用（无 PSRAM） |
| Flash | 16MB |
| 屏幕 | 4.26" E-Ink，SSD1677，800×480 黑白 |
| 存储 | MicroSD，SPI 模式 |
| 输入 | 4 个前键、2 个侧键、电源键 |

### GPIO

```
显示屏:  SCLK=GPIO8  MISO=GPIO7  MOSI=GPIO10  CS=GPIO21  DC=GPIO4  RST=GPIO5  BUSY=GPIO6
SD 卡:   CS=GPIO12
按键:    前按钮组=GPIO1(ADC1_CH1)  侧按钮组=GPIO2(ADC1_CH2)  电源键=GPIO3(active low)
```

显示屏和 SD 卡共享 SPI2 总线，通过不同 CS 引脚区分。

### ADC 按键阈值

| 按键 | 前按钮组 (GPIO1) | 侧按钮组 (GPIO2) |
|------|-------------------|-------------------|
| Back | 3100–3800 | — |
| Confirm | 2090–3100 | — |
| Left | 750–2090 | — |
| Right | 0–750 | — |
| Up | — | 1120–3800 |
| Down | — | 0–1120 |
| 无按下 | ≥ 3800 | ≥ 3800 |

## 按键操作

### 文件浏览器

| 操作 | 按键 |
|------|------|
| 上/下一项 | 侧上 / 侧下 或 Left / Right |
| 打开文件/进入目录 | Confirm |
| 返回上级目录 | Back |
| 打开设置 | 长按 Confirm |
| 随机打开文件 | 长按 Back |

### 阅读界面

| 操作 | 按键 |
|------|------|
| 上/下一页 | 侧上 / 侧下 或 Left / Right |
| 返回文件列表 | Back |
| 返回阅读历史（上一步） | Left |
| 重新渲染当前页 | Confirm |
| 选中/取消 wiki 链接 | 长按 Confirm |
| 跳转到选中的链接 | Confirm（有链接选中时） |
| 进入睡眠 | 长按电源键 2 秒 |

## SD 卡布局

```
/sdcard/
├── vault/                     ← vault 内容（本地或 S3 同步）
│   ├── Daily/
│   ├── Projects/
│   ├── .rr_cache/             ← 图片解码缓存
│   ├── .rr_reader.conf        ← 浏览器根目录配置
│   └── ...
├── remotely_save.conf          ← S3 同步配置
├── wifi.conf                   ← WiFi 凭据
└── ...
```

## S3 同步配置

配置文件按以下顺序查找（找到第一个即停止）：

1. `/sdcard/remotely_save.conf`
2. `/sdcard/remotely_save.txt`
3. `/sdcard/vault/remotely_save.conf`

文件最大 16KB，支持两种格式：

### 格式一：键值对（推荐）

```ini
endpoint=https://oss-cn-hangzhou.aliyuncs.com
region=cn-hangzhou
access_key_id=LTAI5tXXXXXXXXXXXXXX
secret_access_key=XXXXXXXXXXXXXXXXXXXXXXXX
bucket=my-obsidian-vault
# 以下可选
remote_prefix=obsidian/
force_path_style=false
```

| 字段 | 必填 | 说明 |
|------|------|------|
| `endpoint` / `s3_endpoint` | ✅ | S3 兼容端点 URL |
| `region` / `s3_region` | ✅ | 区域，如 `cn-hangzhou` |
| `access_key_id` / `s3_access_key_id` | ✅ | Access Key ID |
| `secret_access_key` / `s3_secret_access_key` | ✅ | Secret Access Key |
| `bucket` / `bucket_name` / `s3_bucket_name` | ✅ | 存储桶名称 |
| `remote_prefix` / `prefix` | ❌ | 远程路径前缀 |
| `force_path_style` | ❌ | 路径风格访问（`true`/`false`），默认 `false` |

支持 `#` 和 `//` 开头的注释行，值可以用单引号或双引号包裹。

### 格式二：Obsidian remotely-save 深度链接

如果使用 Obsidian [remotely-save](https://github.com/remotely-save/remotely-save) 插件，可直接粘贴导出的深度链接：

```
obsidian://remotely-save?data=%7B%22s3%22%3A%7B...
```

### WiFi 配置

配置文件按顺序查找：

1. `/sdcard/wifi.conf`
2. `/sdcard/wifi.txt`
3. `/sdcard/vault/wifi.conf`

```ini
ssid=MyWiFi
password=MyPassword
```

也可通过设备上 **设置 → Wi-Fi 设置** 扫描连接，成功后凭据自动保存。

## 构建与刷机

需要安装 [espup](https://github.com/esp-rs/espup) 和 `espflash`：

```bash
# 在 rr_reader 目录下
cargo run --release
```

此命令会构建固件并通过 `espflash flash --monitor` 刷入设备并打开串口监视器。

```bash
cargo fmt      # 格式化
cargo check    # 类型检查
```

### 字体生成

字体使用自定义 FNT2 压缩点阵格式，通过 `tools/generate_font.py` 从 TTF 生成。

当前字体规格：

| 字体 | 字号 | 用途 | 字符集 |
|------|------|------|--------|
| ui | 20px | 文件列表、设置、WiFi UI | CJK |
| reader | 23px | 正文阅读 | CJK |
| math | 14px | 公式主体 | Latin、Greek、数学符号 |
| script | 12px | 上下标 | Latin、Greek、数学符号 |

## 设计说明

### 内存策略

ESP32-C3 仅有约 380KB 堆内存，必须精打细算：

- **静态帧缓冲**：384KB 的显示帧缓冲放在 `.bss` 段，不占用堆
- **字形缓存**：12 条目 LRU 缓存，复用解压缓冲区
- **分页缓存**：Reader 将解析后的页面序列化到 SD 卡，二次打开无需重新解析整个文件
- **图片缓存**：JPEG 解码后的单色位图持久化到 SD 卡
- **流式 S3**：XML 解析和分块下载均为流式，不将完整响应加载到内存
- **同步清单**：增量写入临时文件，定期原子替换，避免内存中持有完整清单
- 关键路径有堆诊断日志，方便发现内存泄漏

### 公式走轻量自研路线

设备无 PSRAM、黑白墨水屏，集成完整 TeX/KaTeX 成本过高。当前实现覆盖了 Obsidian 笔记中最常见的块公式结构，后续按实际笔记补语法。

## 尚未实现

- 完整 TeX 排版：矩阵、自动换行、复杂定界符伸缩
- 粗体/斜体/删除线视觉区分
- Obsidian callout
- 标签解析与显示
- 阅读进度保存
- 自动定时同步
