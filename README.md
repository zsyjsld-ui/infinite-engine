# 无限引擎 · Infinite Engine

面向复古2.5D联网游戏的Rust引擎。MIT许可证，当前 **0.2.0-dev.1** 开发快照。

仓库包含服务端核心模块与合成示例。游戏业务、客户端、地图、美术、账号和部署配置由应用提供。

|Crate|职责|
|---|---|
|infinite-core|整数实体、移动/交错网格碰撞、预算寻路、战斗/属性/效果/AI、空间查询、增量同步和公平发送预算|
|infinite-runtime|单一权威World的移动调度、输入合并、控制纪元、租约、停止与路线检查点|
|infinite-protocol|地图校验、NM02输入/权威状态、ACK差分与可见流握手|
|infinite-server|直接调用Rust运行时的独立WebSocket移动参考服务|
|infinite-store|有界日志、检查点、后台写入及恢复核心|

**网络参考服务当前接通移动与基础可见同步。** 战斗和持久化库不等于已经接入该服务；正式业务认证、TLS部署、完整游戏及生产容量仍需应用集成验证。此快照不是稳定发行版。

## 构建

需要Rust/Cargo **1.85或更高版本**（edition 2024）。

~~~sh
cargo check --workspace --all-targets --locked
cargo run -p infinite-core --example movement
cargo run -p infinite-core --example queued_navigation
cargo test -p infinite-runtime -p infinite-protocol -p infinite-server --lib --tests --locked
~~~

交互式queued_navigation支持 nav 角色 x y、tick 20、cancel 角色、stop 角色、status和quit。各crate的examples使用合成数据，部分会创建被Git忽略的.local目录。

## 启动独立服务

Node.js 22+内置模块仅用于生成原创几何地图和随机合成凭据，不参与服务器模拟：

~~~sh
node tools/create-demo.mjs
~~~

执行输出的cargo命令。服务只绑定127.0.0.1，端口0自动分配，就绪文件包含端口/PID。向输出的停止文件写入任意内容可正常退出并保存指标。

可以用公开的Rust协议探针验证移动和停止：

~~~sh
cargo run -p infinite-server --example smoke -- ws://127.0.0.1:实际端口/ws .local/demo-实际目录/accounts.txt
~~~

这是协议服务，**不自行提供网页**。应用连接/ws，发送匹配Origin及生成的32字节token完成NM02握手。不要提交合成凭据、日志或正式存档。

## 文档

- [模块边界](docs/CORE.md)
- [协议与地图格式](docs/PROTOCOL.md)
- [持久化](docs/PERSISTENCE.md)
- [MIT 许可证](LICENSE)

第三方依赖使用各自许可证。

## 支持项目

如果无限引擎对你有帮助，欢迎通过[爱发电](https://ifdian.net/a/infinite_engine)支持我的开发与维护工作。捐赠完全自愿，不影响你按 MIT 许可证使用本项目。

也欢迎通过提交问题、改进文档或贡献代码参与项目。感谢每一份支持！
