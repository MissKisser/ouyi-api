//! 协议数据模型层：集中定义上游 API 的请求/响应结构以及欧易渠道的报文结构。
//! 各子模块按协议划分——anthropic 为 Anthropic Messages 协议，openai 为 OpenAI
//! Chat/Response 协议，ouyi / ouyi_v7 为欧易渠道报文，catalog 为模型目录，
//! mapper 负责不同协议之间的结构转换。路由与处理层只依赖本目录暴露的类型。

/// Anthropic Messages 协议的请求、响应与流式事件模型。
pub mod anthropic;
/// 模型目录结构及其从欧易 8.0 / 7.0 数据源的构建逻辑。
pub mod catalog;
/// 客户端短名与欧易完整模型名之间的双向映射器。
pub mod mapper;
/// OpenAI Chat / Responses 协议的请求、响应与流式 chunk 模型。
pub mod openai;
/// 欧易 8.0（Tmpl）协议的报文结构。
pub mod ouyi;
/// 欧易 7.0 协议的报文结构。
pub mod ouyi_v7;
