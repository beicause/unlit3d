
## 编码约定

- **项目依然在极早期开发阶段，不怕兼容性破坏**，注重架构和API设计，不怕影响全仓库的大重构，不怕破坏已有API，没用的API要及时删掉。
- **慎重考虑添加简短的包装函数/帮助函数/getter、setter 函数**。倾向于不添加的情况：
  1. 能通过暴露结构体字段或调用其他更广义的函数实现同等功能，如暴露Vec、暴露更通用统一的函数，而不是添加 push、extend 等函数、添加功能子集函数。
  2. 该函数或功能不常用，节省不了几行代码。
  3. 结构体字段无需只读封装，没必要添加 getter、setter。
- **尽量采用通用方法而不是给内置功能特权**，要考虑功能的通用性，便于用户使用本库进行自定义和拓展。本库的一些内置实现（如unlit渲染）不应该拥有特权和内部专用实现，内部实现应该挪到外部以保证本库的可自定义性和可拓展性。
- **着色器结构体布局用 `glam` + `const_shader_layout`**，在编译期对照 WGSL 对齐规则校验。对于 SSBO ，用`const_shader_layout::ShaderLayout`，对于 UBO 用 `const_shader_layout::ShaderLayoutCompat` 。信任 `const_shader_layout` 的结果，无需额外添加对布局和大小的断言。
- **不要用字面量硬编码可能会变的常量**，如缓冲大小、顶点属性大小、纹理像素大小、结构体大小、字节数组索引等，可用`size_of`、`VertexFormat::size`、`TextureFormat::block_copy_size`、`ShaderLayout::SIZE`等计算。
- **减少不必要的内存分配**，例如：谨慎考虑返回`Vec`的函数，优先考虑返回迭代器或接受`&mut Vec`。遍历迭代器，避免不必要地收集到`Vec`再遍历。每帧复用Vec/HashMap缓存而不是重新创建。
- **字节转换统一走 zerocopy**。
- **可执行程序（`[[bin]]`/`[[example]]`）的名字用连字符（`-`）连接**，包名与库名保持下划线。
- **单元测试和集成测试**：对于较复杂、易错的函数逻辑要添加单元测试，对于各个库的功能添加集成测试，对于整体渲染的正确性添加快照测试。具体测试所在目录参见根目录 [`README.md`](./README.md) 的「测试与基准」一节。对于发现的bug或回归问题，要告知用户，并尽可能添加针对性测试。
- **绕开驱动/后端 bug 时，要在绊到它的那一处绕，并把原因记下来**：症状、成因、绕法与代价写进根目录的 [`WORKAROUND.md`](./WORKAROUND.md)（中文版 [`WORKAROUND.zh-CN.md`](./WORKAROUND.zh-CN.md)），绕过代码旁留一行注释指回该文档。不得靠放宽快照阈值、重存快照或跳过测试把后端差异掩盖过去。条目在上游修复进入我们编译所对的版本后删除。

## Git 工作流

- **主分支（`main`）保持线性历史，不要有 merge commit**。合入改动用 `git cherry-pick`、`git rebase` 或 `git merge --squash`，不要用 `git merge`。

## 提交信息格式约定

提交信息需满足以下格式：
```
<type>[(optional scope)]: <description>

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

- 对于仅文档的任何改动（包括错别字、`cargo doc`修复），一律使用`docs`，不用`fix`。
- 冒号前一律不使用`!`，即 Conventional Commits 里的「破坏性变更」标记。原因：项目处于早期，完全不管兼容性破坏。

## 关于本项目

- **设计文档分散在各`README`中**：根[`README.md`](./README.md)记动机、功能与编码原则，各 crate 的`README.md`记该 crate 的设计抉择与架构。
  它们要随代码一并更新，保持最新。同时更新中英文版的`README`，不要只更新一版。
- **中文里引号一律用单直角引号（`「」`）**，绝不用双引号（`“”`）或引号（`""`）。
- **代码内的文档注释、git提交信息默认全用英文**。文档注释随代码一并更新，保持最新；注释面向用户，不含无关上下文、特定于用户与Agent的会话内容、显而易见的细节等。
- **不要看docs/internal/**。这里面是 WIP 的实施细节，极易过时且可能存在错误。
- **测试生成的二进制快照文件一律放`unlit3d_asset_files`子模块中**。
- **快照不匹配的帧写在 `target/` 下，每一趟各占一个目录**：`cargo xtask test` 的默认档位在 `target/snapshot-mismatches`，`UNLIT3D_DEVICE_TIER=webgl2` 那趟在 `target/snapshot-mismatches-webgl2`，`cargo xtask test-wasm` 在 `target/snapshot-mismatches-wasm`。单独跑测试时，目录由 `UNLIT3D_SNAPSHOT_MISMATCH_DIR` 指定，未设置则用 `target/snapshot-mismatches`。
- **调试 examples 时，可以用 unlit3d-mcp 技能，绝不用截屏软件**。
- **跑测试用 `cargo nextest run`，不用 `cargo test`**。唯一例外是 doctest：nextest 不跑 doctest，那部分只能用 `cargo test --doc`，而 `cargo xtask test` 已经把两者都包了。
- **本地跑 cargo 时，不用设 RUSTFLAGS**，遵循本地 cargo 配置即可，不要照抄 CI 设置 RUSTFLAGS `debuginfo=0`，设置它会让编译缓存失效。
- **常用命令见 `cargo xtask --help`**，以及 `xtask` 包目录的 [`README.md`](./xtask/README.md)。
- **优化二进制程序编译/链接的时间和空间**：本库为编译时间和产物磁盘占用做以下优化：
  1. 用单独的crate里做`include_bytes!`，并导出带有`#[inline(never)]`的返回静态切片的函数，如：`unlit3d_asset_bytes`、`unlit3dx_asset_bytes`。原因：避免修改库或应用程序时重复编译`include_bytes!`部分，`include_bytes!`大文件编译较慢。
  2. 每个crate的集成测试都合并为一个。原因：减少二进制程序产出数量，能大幅减少链接时间和磁盘空间。
