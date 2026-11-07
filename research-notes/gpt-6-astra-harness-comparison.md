# GPT-6 Astra 在 Claude Code 与 Codex 中的表现：简短公开研究

检索日期：2026-10-28。范围：公开网页、官方文档、研究作者的论文与代码仓库；不进行本地模型测试。

## 结论

**本次搜索未发现可核实的、把同一个 GPT-6 Astra 后端分别放入 Claude Code 与 Codex CLI 并控制关键变量的已完成公开评测。** 因此目前不能据本次证据回答哪一个更好或更差。这是“搜索未发现”，不是断言这类报告不存在。

## 1. 名称是否可靠？

- **找到可读取的官方型号文档，而非只有搜索摘要。** OpenAI 模型页明确列出名称 `GPT-6 Astra` 与 `Model ID: gpt-6-astra`；所读页面未提供日期化快照 ID。[OpenAI 模型文档](https://developers.openai.com/api/docs/models/gpt-6-astra)
- AWS 官方模型卡独立列出 OpenAI — GPT-6 Astra，以及 Bedrock ID `openai.gpt-6-astra`。[AWS 模型卡](https://docs.aws.amazon.com/bedrock/latest/userguide/model-card-openai-gpt-6-astra.html)
- **这不验证当前网关实际调用的模型。** 对 `"gpt-6-astra" "hanhan"` 的检索没有找到可验证的映射说明。`hanhan/gpt-6-astra` 可能包含提供方或网关命名空间，但其含义和后端映射尚未核实；不能仅凭会话环境的“powered by”、客户端显示名称或模型自报身份确认后端。这里的推论仅是：官方型号存在，与具体请求是否抵达该型号是两个问题。
- 两个 OpenAI 发布文章地址抓取返回 403，未将其搜索摘要当作已读正文。可读取的官方系统卡也提到 Astra，但它展示的 Codex 模拟是跨模型比较，不是 Claude Code 对照。[系统卡](https://deploymentsafety.openai.com/gpt-6-astra)

## 2. 最接近的线索，为什么仍不是直接证据？

| 一手来源 | 核实结果 | 不能证明什么 |
| --- | --- | --- |
| [Artificial Analysis：Benchmarking GPT-6 Astra](https://artificialanalysis.ai/articles/benchmarking-gpt-6-astra) | 正文报告 Astra **in Codex** 为 62，Fable 5.1 **in Claude Code** 为 62；未给出 Astra/Claude Code 对照，也未明确这里的 Codex 是 CLI。 | 不同模型，即使同分也不能分离 harness 效应。搜索摘要曾出现不同数字，故仅采用抓取正文，不用排行榜证明框架高下。 |
| [pagehub-benchmarks PR #29](https://github.com/pagehub-io/pagehub-benchmarks/pull/29) | 已合并的 Codex CLI adapter，强调共享基准设施和相同构建提示词；所读部分主要是集成、测试和探针，提及的便宜探针使用 GPT-5.6 Sol。 | 在已读部分没有 Astra 双 harness 的完成结果或成对分数；Claude Code 端是否同一模型也未建立。PR 很长，只读了工具返回的前 100,000 字符，不能断言其余讨论没有结果。 |

## 3. 相关但非直接的 harness 研究

### GameASG-Bench：同模型、正好比较这两个 CLI，但模型不是 Astra

研究作者论文 §4.5 / Table 8 使用 **DeepSeek-V4-Flash**，比较 Claude Code 2.1.206 与 Codex CLI 0.153.4，在相同的 47 个游戏生成任务上各运行一次。两边严格成功率都为 **18/47（38.3%）**，但仅有 10 个共同成功；另外各有 8 个只有一方成功。

控制项包括同模型、最高推理设置、相同任务包与评估流程、全新会话和干净工作区。限制：每配置每任务仅一次；Claude Code 的名义 120-turn 预算未见对应 Codex 限制说明；比较的是整套 harness（提示、工具、上下文处理、协议等），不是单一组件。论文还报告 Astra/Codex CLI 为 26/47，但没有 Astra/Claude Code 行，不能拿它与其他模型行比较来回答本题。

**可支持的结论：** 对这一模型和任务集，整体成功率相同仍可能伴随明显的逐任务差异；不能外推 Astra 或一般编程任务。

来源：[作者论文全文](https://arxiv.org/html/2609.21293v1)。

### TxBench-PP：同 checkpoint 的 harness 效应，但不是这两个工具互比

药理学任务研究固定 checkpoint、题目和提交程序，比较 **Pi 对 Claude Code**（Claude 模型）及 **Pi 对 Codex**（GPT 模型）。每配置 100 题、每题 3 次。作者报告 Pi 相对 Claude Code 的合并差异为 +4.4 个百分点（95% CI 1.2–7.8），相对 Codex 为 +5.5（1.2–9.8）。未测试 Astra，也没有同模型 Claude Code 对 Codex 的配对。

重要混杂：论文所测 Codex 配置缺乏视觉能力，图片呈现也因 harness 而异。因此它说明模型与运行环境组合值得测量，而不是证明 Claude Code 或 Codex 普遍占优。

来源：[作者论文全文](https://arxiv.org/html/2606.19245v1)。

## 4. 检索词、局限与实际判断

主要检索词：

- `"gpt-6-astra"`
- `"GPT-6 Astra" "Claude Code" "Codex" benchmark`
- `"gpt-6-astra" "hanhan"`
- `"Claude Code" "Codex" "same model" benchmark harness`
- `"gpt-6-astra" "Claude Code" "same" benchmark`
- `"Astra" "Claude Code" "Codex CLI" "harness" benchmark`

局限：这是有限公开网页检索，受搜索索引覆盖、网页可访问性和长页截断限制；未穷尽未索引仓库、私有实验或社交平台讨论。论文数据按作者公开报告核对，未复现实验，不将其视作已独立复核的普遍结论。没有用不同模型的 leaderboard 推断 harness 优劣。

若要判断自己的网关配置，建议另做小规模配对实验：先核实提供方与后端版本映射，再固定任务与仓库快照、推理设置、token/时间预算、权限、MCP/skills 和网关转换版本，记录两个 CLI 版本；对同任务重复运行，比较测试通过率、成本、延迟及失败轨迹。无法核实后端时，只能把结果称为“两套实际配置”的比较。

## 笔记位置

最少量检查了仓库顶层目录与 `review-handoff/README.md`：后者是路由器网络功能的临时评审交接，不是通用 LLM 研究目录。因此本笔记单独放在 `research-notes/`，不混入产品 docs；未修改其他文件或编译产品。
