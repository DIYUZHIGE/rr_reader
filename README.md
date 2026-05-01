# rr_reader

在阅星瞳 X4 墨水屏设备上离线浏览一个 Obsidian vault。

当前目标很具体：把 Obsidian vault 放到 SD 卡的 `/vault` 目录，设备启动后递归扫描其中的 Markdown 文件，在墨水屏上显示文件列表，选择文件后显示内容。

## 当前状态

已实现：

- ESP32-C3 + ESP-IDF Rust 工程启动
- SD 卡挂载，挂载点为 `/sdcard`
- 递归扫描 `/sdcard/vault` 下的 Markdown 文件
- 支持 `.md`、`.markdown`，扩展名大小写不敏感
- SSD1677 800x480 墨水屏驱动
- 共享 SPI2 总线：显示屏和 SD 卡共用总线，不同 CS
- 多字号压缩点阵字体：16px UI、18px 阅读支持 CJK，12px math 和 10px script 支持 Latin、Greek 和常用数学符号
- 文件浏览器：显示文件列表、当前选中项、文件总数
- 基础 Reader：打开 Markdown 文件并按屏幕尺寸分页显示
- Reader 翻页：侧键或左右键在当前文件内翻页
- 基础 Markdown 渲染：标题、列表、任务列表、引用、代码块、分割线、表格简化显示
- 公式渲染：支持识别 `$...$` 行内公式和 `$$...$$` 块公式；支持 `^`、`_`、`\frac`、`\sqrt` 的轻量二维布局，并替换常见希腊字母和数学符号
- 图片显示：支持 Markdown 图片、Obsidian `![[...]]` 图片嵌入和 JPEG 解码，其他图片格式显示占位框
- 按键输入：前键、侧键、电源键，参考 crosspoint 的 ADC 阈值和去抖模型
- 深度睡眠入口：长按电源键进入睡眠

还没实现：

- 完整 TeX / LaTeX 公式排版：暂不支持矩阵、自动换行、复杂定界符伸缩等完整 TeX 布局
- 阅读进度保存
- Obsidian wiki link 跳转
- 目录树浏览
- S3 / remotely-save 同步
- WiFi 配置

## SD 卡布局

把 Obsidian vault 放到 SD 卡根目录下的 `vault` 文件夹：

```text
/vault/
├── Daily/
│   └── 2026-05-01.md
├── Projects/
│   └── rr_reader.md
└── index.md
```

设备启动后会扫描：

```text
/sdcard/vault
```

扫描规则：

- 递归扫描所有子目录
- 只显示 `.md` 和 `.markdown`
- 扩展名大小写不敏感，例如 `.MD` 也会识别
- 列表里显示的是相对 `/sdcard/vault` 的路径

## 按键

文件浏览器：

| 操作 | 按键 |
|------|------|
| 下一项 | 侧下 / Right |
| 上一项 | 侧上 / Left |
| 打开文件 | Confirm |
| 长按滚动 | 侧键或 Left/Right 长按 |

阅读界面：

| 操作 | 按键 |
|------|------|
| 返回文件列表 | Back / Left |
| 下一页 | 侧下 / Right |
| 上一页 | 侧上 / Left |
| 重新渲染当前文件 | Confirm |
| 睡眠 | 长按电源键 |

## 硬件

目标设备：阅星瞳 X4。

已按 crosspoint-reader 实测参数配置：

| 部件 | 参数 |
|------|------|
| MCU | ESP32-C3 |
| RAM | 无 PSRAM，约 380KB 可用 |
| Flash | 16MB |
| 屏幕 | 4.26" E-Ink，SSD1677，800x480，黑白 |
| 存储 | MicroSD，SPI |
| 输入 | 4 个前键、2 个侧键、电源键 |

GPIO：

```text
显示屏:
  SCLK  GPIO8
  MISO  GPIO7
  MOSI  GPIO10
  CS    GPIO21
  DC    GPIO4
  RST   GPIO5
  BUSY  GPIO6

SD 卡:
  CS    GPIO12

按键:
  前按钮组  GPIO1 / ADC1_CH1
  侧按钮组  GPIO2 / ADC1_CH2
  电源键    GPIO3，active low
```

ADC 阈值来自 crosspoint 实测：

| 按键 | ADC 范围 |
|------|----------|
| Back | 3100-3800 |
| Confirm | 2090-3100 |
| Left | 750-2090 |
| Right | 0-750 |
| Up | 1120-3800 |
| Down | 0-1120 |
| 无按下 | >= 3800 |

## 构建和刷机

在 `rr_reader` 目录运行：

```bash
cargo run --release
```

这会构建固件，并通过 `espflash flash --monitor` 刷入设备和打开串口监视器。

常用检查：

```bash
cargo fmt
cargo check
```

## 设计取舍

### 先做本地 vault，再做同步

这个项目最终可以接 Obsidian remotely-save 的 S3 配置，但当前阶段优先保证本地阅读闭环：

1. SD 卡能稳定读取 vault
2. 文件列表能正确显示
3. 按键选择和打开文件可靠
4. Markdown 能分页阅读
5. 阅读进度能保存

同步功能会放到阅读体验稳定之后再做，否则 SD、显示、按键、网络问题会混在一起，不利于调试。

### 当前 Reader 是轻量 Markdown 分页器

现在 Reader 使用 `pulldown-cmark` 解析 Markdown，再按字体宽度和屏幕高度分页。当前重点是把常见 Obsidian 笔记以稳定、低内存的方式显示出来，因此渲染能力是取舍后的子集：

- 支持标题、列表、任务列表、引用、代码块、分割线、表格简化显示。
- 支持识别 Obsidian / TeX 风格的 `$...$` 和 `$$...$$`；行内公式和块公式使用 12px math 字体和 10px script 字体绘制 `^`、`_`、`\frac`、`\sqrt` 的轻量二维布局。
- 支持 Markdown 图片和 Obsidian `![[...]]` 图片嵌入；JPEG 会尝试解码显示，其他格式显示图片占位框。
- 不支持完整富文本排版，粗体/斜体/删除线目前不会表现为不同字形。
- 不支持完整 TeX / LaTeX 公式排版；矩阵、自动换行、复杂定界符伸缩等高级布局还未实现。

公式支持走轻量自研路线。设备是 ESP32-C3、无 PSRAM、黑白墨水屏，直接集成完整 TeX 或 KaTeX 级别的布局/字体系统成本较高。当前实现优先覆盖 Obsidian 笔记里最常见的块公式结构，后续再按实际笔记补语法。

## 下一步

建议优先级：

1. 修稳按键和刷新：确认侧键、前键、Half refresh 在真机上都可靠。
2. 保存阅读进度：记录每个文件最后阅读页。
3. 文件浏览器目录化：支持文件夹展开/进入，而不是扁平路径列表。
4. Markdown 渲染补齐：链接文本、粗体/斜体/删除线的视觉区分。
5. Obsidian 特性：`[[wiki link]]`、标签、callout。
6. 公式支持：补齐矩阵、复杂定界符伸缩和公式自动换行。
7. WiFi + S3 同步：读取 remotely-save 配置并下载 vault。
