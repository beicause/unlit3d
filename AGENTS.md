
## 编码约定

- **项目依然在极早期开发阶段，不怕兼容性破坏**，注重代码质量、解法的干净程度、架构和 API 设计，不怕影响全仓库的大重构，不怕破坏已有 API，没用的 API 要及时删掉。
- **慎重考虑添加简短的包装函数/帮助函数/getter、setter 函数**。倾向于不添加的情况：
  1. 能通过暴露结构体字段或调用其他更广义的函数实现同等功能，如暴露 Vec、暴露更通用统一的函数，而不是添加 push、extend 等函数、添加功能子集函数。
  2. 该函数或功能不常用，节省不了几行代码。
  3. 结构体字段无需只读封装，没必要添加 getter、setter。
- **尽量采用通用方法而不是给内置功能特权**，要考虑功能的通用性，便于用户使用本库进行自定义和拓展。本库的一些内置实现（如 unlit 渲染）不应该拥有特权和内部专用实现，内部实现应该挪到外部以保证本库的可自定义性和可拓展性。
- **着色器结构体布局用 `glam` + `const_shader_layout`**，在编译期对照 WGSL 对齐规则校验。对于 SSBO，用 `const_shader_layout::ShaderLayout`；对于 UBO，用 `const_shader_layout::ShaderLayoutCompat`。信任 `const_shader_layout` 的结果，无需额外添加对布局和大小的断言。
- **不要用字面量硬编码可能会变的常量**，如缓冲大小、顶点属性大小、纹理像素大小、结构体大小、字节数组索引等，可用 `size_of`、`VertexFormat::size`、`TextureFormat::block_copy_size`、`ShaderLayout::SIZE` 等计算。
- **减少不必要的内存分配**，例如：谨慎考虑返回 `Vec` 的函数，优先考虑返回迭代器或接受 `&mut Vec`。遍历迭代器，避免不必要地收集到 `Vec` 再遍历。每帧复用 Vec/HashMap 缓存而不是重新创建。
- **字节转换统一走 zerocopy**。
- **自定义错误类型用 `thiserror` 派生 `Error`**，不要手写 `impl fmt::Display` 与 `impl std::error::Error`。
- **可执行程序（`[[bin]]`/`[[example]]`）的名字用连字符（`-`）连接**，包名与库名保持下划线。
- **单元测试和集成测试**：对于较复杂、易错的函数逻辑要添加单元测试，对于各个库的功能添加集成测试，对于整体渲染的正确性添加快照测试。具体测试所在目录参见根目录 [`TESTING.md`](./TESTING.md)。对于发现的 bug 或回归问题，要告知用户，并尽可能添加针对性测试。
- **单份代码文件不得达到 1000 行**：文件过长，意味着多套互不相干的逻辑共处一处，局部读取易致漏读与误判。超了按职责拆分，`cargo xtask line-count` 会列出达到上限的文件。
- **绕开驱动/后端 bug 时，要在绊到它的那一处绕，并把原因记下来**：症状、成因、绕法与代价写进根目录的 [`WORKAROUND.md`](./WORKAROUND.md)（中文版 [`WORKAROUND.zh-CN.md`](./WORKAROUND.zh-CN.md)），绕过代码旁留一行注释指回该文档。

## Git 工作流

- **主分支（`main`）保持线性历史，不要有 merge commit**。合入改动用 `git cherry-pick`、`git rebase`、`git merge --ff-only` 或 `git merge --squash`，不要用 `git merge`。

## 提交信息格式约定

提交信息需满足以下格式：
```
<type>[(optional scope)]: <description>

[optional body]

[optional footer(s)]
```
提交信息用英文，不得含 emoji。`<description>` 首字母大写，末尾不带句号。`body` 与 `footer` 首字母大写，末尾带句号。其中 `<type>` 可以是：
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

- 对于仅文档的任何改动（包括错别字、`cargo doc` 修复），一律使用 `docs`，不用 `fix`。
- 冒号前一律不使用 `!`，即 Conventional Commits 里的「破坏性变更」标记。原因：项目处于早期，完全不管兼容性破坏。

## 关于本项目

- **设计文档分散在各 `README` 中**：根 [`README.md`](./README.md) 记动机、功能与编码原则，各 crate 的 `README.md` 记该 crate 的设计抉择与架构，测试、基准与 CI 记在 [`TESTING.md`](./TESTING.md)。
  它们要随代码一并更新，保持最新。同时更新中英文版，不要只更新一版。
- **`.agents/skills/unlit3d/` 里的技能文档随代码一并更新**：它是 Agent 写场景、查 API、调试示例的第一手依据，签名与约定过时会让 Agent 照旧名字写出编不过的代码，新子系统没人告诉它就等于不存在。改到渲染管线、光照、附件与目标特化、帧与输入、MCP 工具、多 world 装配这些面时，顺手核对对应那一篇；新增子系统的同时补一节。
- **中文里引号一律用单直角引号（`「」`）**，绝不用双引号（`“”`）或引号（`""`）。
- **代码内的文档注释、git 提交信息默认全用英文**。文档注释随代码一并更新，保持最新；注释面向用户，不含无关上下文、特定于用户与 Agent 的会话内容、显而易见的细节等。
- **不要看 `docs/internal/`**。这里面是 WIP 的实施细节，极易过时且可能存在错误。
- **测试生成的二进制快照文件一律放 `unlit3d_asset_files` 子模块中**。
- **快照不匹配的帧写在 `target/` 下，每一趟各占一个目录**：`cargo xtask test` 的默认档位在 `target/snapshot-mismatches`，`UNLIT3D_DEVICE_TIER=webgl2` 那趟在 `target/snapshot-mismatches-webgl2`，`cargo xtask test-wasm` 的 WebGPU 那趟在 `target/snapshot-mismatches-wasm`、WebGL2 那趟在 `target/snapshot-mismatches-wasm-webgl2`。单独跑测试时，目录由 `UNLIT3D_SNAPSHOT_MISMATCH_DIR` 指定，未设置则用 `target/snapshot-mismatches`。
- **调试 examples 时，可以用 unlit3d 技能里的 MCP 宿主，绝不用截屏软件**。
- **跑测试用 `cargo nextest run`，不用 `cargo test`**。`nextest` 比 `test` 更快，输出格式更友好。唯一例外是 `cargo test --doc`，因为 `nextest` 不跑 doctest。`cargo xtask test` 快捷命令会跑 nextest 和 doctest 两者。
- **本地跑 cargo 时，不用设 RUSTFLAGS**，遵循本地 cargo 配置即可，不要照抄 CI 设置 RUSTFLAGS `debuginfo=0`，设置它会让编译缓存失效。
- **常用命令见 `cargo xtask --help`**，以及 `xtask` 包目录的 [`README.md`](./xtask/README.md)。
- **优化二进制程序编译/链接的时间和空间**：本库为编译时间和产物磁盘占用做以下优化：
  1. 把 `include_bytes!` 放进单独的 crate，并导出带 `#[inline(never)]` 的、返回静态切片的函数，如 `unlit3d_asset_bytes`、`unlit3dx_asset_bytes`。原因：避免修改库或应用程序时重复编译 `include_bytes!` 部分——`include_bytes!` 大文件编译较慢。
  2. 每个 crate 的集成测试都合并为一个。原因：减少二进制程序产出数量，能大幅减少链接时间和磁盘空间。

## 文档与注释的常见错误

下列错法在写或改注释、说明文档时反复出现。每条给的是判断方法，不是泛泛的原则；对照时以「读者能否据此正确理解」为准。

- **把沿革当成说明**：写「原先」「已改成」，或英文里的 `used to`、`ported from`，只在叙述代码此前是什么。沿革留给提交历史；正文要写此刻的契约，以及为何是这样——哪条约束排除了显而易见的另一种做法、换来什么、代价是什么。判据：这句话删掉后，读者会误解现状或选错用法吗？会就改成交代取舍，不会就删。
- **描述与代码不符**：写「每帧都做」，实际有前置条件才做；写「完整清单见某节」，那节并没有这份清单。动笔前读一遍被描述的那段代码，注意提前返回、条件编译和上限常量。
- **同一机制在多处复述**：机制在拥有它的那一处讲透，别处只留一句引用。判据：这段能否换成一句链接而不丢信息？能就换。
- **精简时删掉唯一记载**：把一段压成「详见某处」之前，先确认那处真的接得住被删的每一条事实；接不住就先补过去，或保留原文。
- **引用过时**：指向的文件、小节、测试位置、环境变量、命令行开关已改名或根本不存在。改代码时顺手核对这些指代。
- **把嵌套仓库的内容当成外层仓库的内容**：如果某个目录是从另一个仓库嵌套进来的，它的文件不在外层仓库的版本树中，按外层路径写的链接在代码托管网页上打不开；这类链接要指向内容真正所在的仓库。
- **链接只在部分页面里成立**：同一份链接在文档生成器、代码托管网页、包索引站点上未必都能解析——有的语法只有文档生成器认，有的目标被条件编译挡住。要同时供几处读的文件，用处处都能解析的写法。
- **同一份文档的多个语言版本不同步**：只改一版，或两版事实不一致。事实必须一致，措辞按各自语言的习惯。
- **把工作过程写进去**：出现「用户要求」「本轮」「上一版」这类只对当次对话有意义的表述。文档面向此后读它的人。
- **复述代码已经说清的**：把签名、类型、显而易见的返回值再说一遍。该写的是为什么这样、代价是什么、取舍在哪里。
- **中英混排与标点**：汉字与拉丁字母、行内代码之间留空格；中文句子用全角标点；引号、破折号等按项目既定排版约定统一，同一份文档内不混用几种写法。
- **提交说明不覆盖实际改动**：说明只提一处，改动里还有别的。提交前看一遍改动清单，把改动说全。
