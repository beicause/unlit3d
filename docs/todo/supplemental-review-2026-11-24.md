# Blitz 输入、字体与纹理数组补审来源记录

六项补审实现任务已经按主计划格式迁移到[主实施计划的 P0 章节](agent-implementation-plan-2026-11-24.md#p0-e)，统一编号为 `P0-E`–`P0-J`。本文件只保留补审范围、证据和来源说明，不再维护第二套任务正文或独立状态。这六项已经实施、验收并提交，逐包记录见主计划的[实施进度](agent-implementation-plan-2026-11-24.md#实施进度)。

补审覆盖 [`crates/unlit3d/src/blitz/events.rs`](../../crates/unlit3d/src/blitz/events.rs)、[`crates/unlit3d/src/blitz/fonts.rs`](../../crates/unlit3d/src/blitz/fonts.rs) 与 [`crates/unlit_wgpu/src/texel_array.rs`](../../crates/unlit_wgpu/src/texel_array.rs)，并追踪输入折叠、BlitzSource、文档构造、staging 上传和现有测试。详细问题、实施步骤、测试与验收以主计划中的 `P0-E`–`P0-J` 为唯一权威来源。

来源会话：`session-86e5fd52-0e09-4b43-887e-1c31f09c5681`。补审使用锁定依赖 `blitz-dom 0.3.0-beta.4`、`fontique 0.12.0`；隔离探针只复现了当前错误行为，不替代正式 Cargo/GPU/wasm 回归测试。实施仍必须遵守主计划的统一争议决策、责任分工和关闭门槛。
