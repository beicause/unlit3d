
## 编码约定

- **项目依然在极早期开发阶段，不用担心兼容性破坏**，注重架构和API设计，不要怕改动已有API，并且没用的API要及时删掉。
- **尽量采用通用方法而不是给内置功能特权**，要考虑功能的通用性，便于用户使用本库进行自定义和拓展。本库的一些内置实现（如unlit渲染）不应该拥有特权和内部专用实现，内部实现应该挪到外部以保证本库的可自定义性和可拓展性。
- **着色器结构体布局用 glam + `const_shader_layout`**，在编译期对照 WGSL 对齐规则校验。对于 SSBO ，用`const_shader_layout::ShaderLayout`，对于 UBO 用 `const_shader_layout::ShaderLayoutCompat` 。信任 `const_shader_layout` 的结果，无需额外添加对布局和大小的断言。
- **不要用字面量硬编码可能会变的常量**，如缓冲大小、顶点属性大小、纹理像素大小、结构体大小、字节数组索引等，可用`size_of`、`VertexFormat::size`、`TextureFormat::block_copy_size`、`ShaderLayout::SIZE`等计算。
- **减少不必要的内存分配**，例如：遍历迭代器而不是收集到Vec再遍历、返回迭代其而不是Vec、每帧复用Vec/HashMap而不是重新创建。
- **字节转换统一走 zerocopy**。
- **可执行程序（`[[bin]]`/`[[example]]`）的名字用连字符（`-`）连接**，包名与库名保持下划线。
- **单元测试和集成测试**：对于较复杂、易错的函数逻辑要添加单元测试，对于各个库的功能添加集成测试，对于整体渲染的正确性添加快照测试。具体测试所在目录参见根目录 [`README.md`](./README.md) 的「测试与基准」一节。对于发现的bug或回归问题，要告知用户，并尽可能添加针对性测试。

## Git 工作流

- **主分支（`main`）保持线性历史，不要有 merge commit**。合入改动用 `git cherry-pick`、`git rebase` 或 `git merge --squash`，不要用 `git merge`。

## 提交信息格式约定

提交信息需满足以下格式：
```
<type>[optional scope]: <description>

[optional body]

[optional footer(s)]
```
提交信息用英文，不得含emoji。`<description>`首字母大写，末尾不带句号。`body`和`footer`首字母大小，末尾带句号。其中`<type>`可以是：
| Type       | Purpose                        |
| ---------- | ------------------------------ |
| `feat`     | New feature                    |
| `fix`      | Bug fix                        |
| `docs`     | Documentation only             |
| `style`    | Formatting/style (no logic)    |
| `refactor` | Code refactor (no feature/fix) |
| `perf`     | Performance improvement        |
| `test`     | Add/update tests               |
| `build`    | Build system/dependencies      |
| `ci`       | CI/config changes              |
| `chore`    | Maintenance/misc               |
| `revert`   | Revert commit                  |

## 常用命令

`cargo xtask`约定含有一些常用命令，各命令及其解释见根目录 [`README.md`](./README.md) 的「常用命令」一节；更新`xtask`时，注意同步更新该列表。

- **提交前检查**：让以下命令通过（不一定要最后才运行，若之前运行后没改动，则不必重复运行），尽量一次运行多个命令，减少会话步次：
  1. `cargo fmt --all`。
  2. `cargo xtask check`。
  3. `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items --keep-going`。
  4. `tombi format`、`typos`。

由于Rust编译可能较慢，要注意：
- **先修复格式（`fmt`、`clippy`），再跑cargo编译或测试**，原因：`fmt`/`clippy`格式化速度比较快。`编译->格式化->编译`可能会破坏编译缓存再次全量编译，而`格式化->编译->编译`会命中编译缓存，更有利于再次运行或用户运行验证结果。
- **使用`cargo nextest`，而不是`cargo test`**。
- **针对特定改动、特定bug时，使用`cargo nextest`时要筛选**。不要总是跑全量`cargo nextest`或`cargo xtask test`测试。
- **不要过度跑`cargo build`、`cargo nextest`**：
  1. 开始任务时，假定所有测试均已通过，无需测试。
  3. 对于仅文档的、非常简单的、非逻辑性的改动，不要重跑`cargo nextest`。
  2. 整合多次改动后，再跑检查或测试，不要改一点测一点，减少检查或测试次数，尽可能减少会话轮次。
  4. 优先用`cargo check`或`clippy`，尽量减少编译次数。
  5. 尽量在当前平台上实现测试，不要依赖设备特定行为，使用运行时检测GPU设备功能、`DeviceTier`等配置切换代码路径。如果改动目标不是平台特定的，就绝不跑平台特定（如wasm，android）的检查或测试，特定平台的cargo冷构建通常很慢。
- **不会自己结束的命令要放后台跑，且不要对其等待**：如 `cargo xtask run-wasm`（它启动的静态服务器一直运行到被停止）。这类命令用后台任务启动，之后正常做别的事、需要时再读它的输出，**绝不要对其调用带 `wait` 的读取**——它不会结束，等待只会白等到超时。

## 关于本项目

- **设计文档分散在各`README`中**：根[`README.md`](./README.md)记动机、功能与编码原则，各 crate 的`README.md`记该 crate 的设计抉择与架构。
  它们要随代码一并更新，保持最新。同时更新中英文版的`README`，不要只更新一版。
- **中文里引号一律用单直角引号（`「」`）**，绝不用双引号（`“”`）或引号（`""`）。
- **代码内的文档注释、git提交信息默认全用英文**。文档注释随代码一并更新，保持最新；注释面向用户，不含无关上下文、特定于用户与Agent的会话内容、显而易见的细节等。
- **不要看docs/internal/**。这里面是WIP的实施细节，极易过时且可能存在错误。
- **测试生成的二进制快照文件一律放`unlit3d_asset_files`子模块中**。
