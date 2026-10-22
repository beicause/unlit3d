
## 编码约定

- **项目依然在极早期开发阶段，不怕兼容性破坏**，注重架构和API设计，不怕影响全仓库的大重构，不怕破坏已有API，没用的API要及时删掉。
- **对于公开 API，慎重考虑添加简短的包装函数/帮助函数/getter、setter 函数**。倾向于不添加的情况：
  1. 用户能通过公开的结构体字段或其他更广义的函数实现同等功能。
  2. 该函数或功能不常用。
  3. 结构体字段无需只读封装，考虑直接暴露该字段，不要 getter、setter。
- **尽量采用通用方法而不是给内置功能特权**，要考虑功能的通用性，便于用户使用本库进行自定义和拓展。本库的一些内置实现（如unlit渲染）不应该拥有特权和内部专用实现，内部实现应该挪到外部以保证本库的可自定义性和可拓展性。
- **着色器结构体布局用 glam + `const_shader_layout`**，在编译期对照 WGSL 对齐规则校验。对于 SSBO ，用`const_shader_layout::ShaderLayout`，对于 UBO 用 `const_shader_layout::ShaderLayoutCompat` 。信任 `const_shader_layout` 的结果，无需额外添加对布局和大小的断言。
- **不要用字面量硬编码可能会变的常量**，如缓冲大小、顶点属性大小、纹理像素大小、结构体大小、字节数组索引等，可用`size_of`、`VertexFormat::size`、`TextureFormat::block_copy_size`、`ShaderLayout::SIZE`等计算。
- **减少不必要的内存分配**，例如：谨慎考虑返回`Vec`的函数，优先考虑返回迭代器或接受`&mut Vec`。遍历迭代器，避免不必要地收集到`Vec`再遍历。每帧复用Vec/HashMap缓存而不是重新创建。
- **字节转换统一走 zerocopy**。
- **可执行程序（`[[bin]]`/`[[example]]`）的名字用连字符（`-`）连接**，包名与库名保持下划线。
- **单元测试和集成测试**：对于较复杂、易错的函数逻辑要添加单元测试，对于各个库的功能添加集成测试，对于整体渲染的正确性添加快照测试。具体测试所在目录参见根目录 [`README.md`](./README.md) 的「测试与基准」一节。对于发现的bug或回归问题，要告知用户，并尽可能添加针对性测试。

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

## 常用命令

`cargo xtask`约定含有一些常用命令，各命令及其解释见根目录 [`README.md`](./README.md) 的「常用命令」一节；更新`xtask`时，注意同步更新该列表。

由于Rust编译较慢且占用磁盘，本地机器负担不起，要注意：
- **本地只跑检查类命令**：`cargo fmt --all`、`cargo xtask check`（clippy，含逐包那一趟）、`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items --keep-going`、`tombi format`、`typos`。让这些通过（不一定要最后才运行，若之前运行后没改动，则不必重复运行），尽量一次运行多个命令，减少会话步次。
- **编译与测试一律交给 CI**：`cargo build`、`cargo nextest`、`cargo test`、`cargo xtask test`、`cargo xtask test-wasm`、`cargo xtask build-android`、`cargo bench` 都不在本地跑。需要编译或测试时，把改动推到一个远程分支上即可，`.github/workflows/ci.yml` 会在该分支的每次推送后跑轻量档，不必先开 PR；本地只看它给出的日志。轻量档是 Linux 上的检查与 nextest，加上 wasm 与 Android 的 `check`（不编译二进制、不进浏览器、不打 APK）；全量（macOS/Windows、wasm 浏览器测试、APK）只在 `main` 与 merge queue 上跑，由 `.github/workflows/ci-main.yml` 调用轻量档并补上这些。

开发验证流程遵循以下步骤：
1. 先完成尽可能多的改动。
2. 在本地就地解决检查和格式等问题。
3. 如果远程分支不存在，获得用户许可后才能创建新分支，不要在本地先「试着编译一下」验证，也不要擅自创建新分支或开 PR。
4. 一旦远程分支已经存在，后续推送、验证无需获得许可。
5. CI 失败就从日志里读原因。用 `gh pr checks --watch` 跟进度、`gh run view --log-failed` 读失败的步骤，继续修改、检查、推送，迭代直到 CI 变绿。

## 关于本项目

- **设计文档分散在各`README`中**：根[`README.md`](./README.md)记动机、功能与编码原则，各 crate 的`README.md`记该 crate 的设计抉择与架构。
  它们要随代码一并更新，保持最新。同时更新中英文版的`README`，不要只更新一版。
- **中文里引号一律用单直角引号（`「」`）**，绝不用双引号（`“”`）或引号（`""`）。
- **代码内的文档注释、git提交信息默认全用英文**。文档注释随代码一并更新，保持最新；注释面向用户，不含无关上下文、特定于用户与Agent的会话内容、显而易见的细节等。
- **不要看docs/internal/**。这里面是WIP的实施细节，极易过时且可能存在错误。
- **测试生成的二进制快照文件一律放`unlit3d_asset_files`子模块中**。
- **调试examples时，可以用 unlit3d-mcp 技能，绝不用截屏软件**。
