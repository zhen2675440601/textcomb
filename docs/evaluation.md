# TextComb 质量评测

评测文章与裁决标注不进入公开仓库。公开仓库只保存评测程序、格式说明和不含真实文章的最小示例。

## 标注流程

1. 两名标注者独立阅读完整文章，按错字、标点、八类病句和分段建议标注。
2. 两人不一致的项目交给第三人裁决。
3. `char_start` 与 `char_end` 使用提取后不可变正文的 Unicode 字符下标，范围为左闭右开。
4. 同一处只保留一个裁决问题；问题类别必须唯一。分段建议不进入正式问题准确率承诺。
5. 每次评测锁定模型、提示词、依据库和分析器版本，并保存服务商返回的用量，不保存完整提示词日志。

私有 Gold 文件为 JSONL，每行格式如下：

```json
{"document_id":"article-001","issues":[{"category":"grammar","char_start":12,"char_end":25}]}
```

预测目录中的文件名必须是 `<document_id>.json`，内容为 `textcomb.report.v1` 报告。

```shell
cargo run -p textcomb-eval -- \
  --gold ../textcomb-evaluation/gold.jsonl \
  --predictions ../textcomb-evaluation/reports \
  --output evaluation-summary.json
```

匹配规则为“类别相同且原文范围重叠”的一对一匹配。存在多个重叠项时优先选择交集最长的一对，剩余重复预测计作误报。

发布门槛：

- 正式问题准确率不低于 90%；
- 正式加疑似的召回率不低于 90%；
- 疑似区准确率不低于 70%；
- 固定温度为 0 的参考模型连续两轮均达标。

`textcomb-eval` 分别输出正式、仅疑似、合并结果及合并后的分类型指标。由于“仅疑似准确率”与“所有 Gold 项均可成为正式问题”的关系会受标注政策影响，发布评审还需按内部标注属性生成疑似区专项表。
