# Solos

运行在你自己手机上的个人 AI 助手，支持 iOS 和 Android：连接你选择的模型（OpenAI 兼容接入点、
Anthropic、Gemini），并在 app 内置的 Linux 沙箱里真正动手干活。

所有非界面部分（agent、工具、存储、模型接入）是两个平台共用的一个 Rust 核心；各平台只加自己的界面和设备服务。

> 状态：早期开发中。iOS app 正在开发，Android 尚未开始。范围见 `docs/requirements.md`，
> 设计见 `docs/architecture.md`。

构建步骤、许可证等见英文版 [README.md](README.md)。
