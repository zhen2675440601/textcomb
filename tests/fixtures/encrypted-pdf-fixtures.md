# 加密 PDF 拒绝样例

`owner-encrypted.pdf` 和 `password-encrypted.pdf` 仅由仓库的合成文件 `e2e-sample.pdf` 生成，没有用户文章或第三方文章正文。

- 所有者加密样例使用空用户密码和测试所有者密码 `DisposableReviewOwnerOnly`，允许复制文字；Poppler 可以成功提取，但 TextComb 的首版边界仍要求拒绝。
- 用户密码样例使用测试密码 `ReviewExampleOnly`，正常未解密请求也应返回 `PDF_ENCRYPTED`。
- 密码只属于可公开再分发的测试样例，不是部署凭据。使用 RC4-128 是为了兼容检测工具，不代表生产加密建议。

可用 Python 与 pypdf 从合成 PDF 重新生成，用于覆盖能够提取正文但仍然加密的文件。
