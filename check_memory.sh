#!/bin/bash
# 内存使用检查脚本

set -e

echo "=========================================="
echo "编译固件 (release模式，极限优化)"
echo "=========================================="
cargo build --release

echo ""
echo "=========================================="
echo "固件大小分析"
echo "=========================================="
cargo size --release -- -A

echo ""
echo "=========================================="
echo "最大内存占用分析 (前20项)"
echo "=========================================="
cargo bloat --release -n 20

echo ""
echo "=========================================="
echo "二进制文件大小"
echo "=========================================="
ls -lh target/riscv32imc-esp-espidf/release/rr_reader

echo ""
echo "=========================================="
echo "优化建议"
echo "=========================================="
echo "1. 检查 .data 和 .bss 段大小 - 这些占用RAM"
echo "2. 检查 .text 段大小 - 这占用Flash"
echo "3. 运行时堆使用需要在设备上测试"
echo "4. 使用 'cargo bloat' 找出最大的函数"
echo ""
echo "运行时内存监控："
echo "  - 启用 debug_assertions 编译"
echo "  - 查看日志中的 'Free heap' 信息"
echo "  - 特别关注 markdown 解析时的内存使用"
