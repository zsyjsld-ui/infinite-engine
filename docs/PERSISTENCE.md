# 持久化边界

infinite-store提供二进制事务日志、提交边界、校验、后台有界交接及检查点恢复。应用决定记录、提交时机、失败重试和迁移；排队成功不能当作持久提交成功。

~~~sh
cargo run -p infinite-store --example journal
cargo run -p infinite-store --example background
~~~

示例/测试使用独立合成目录。fault-injection特性默认关闭。正式存档、账号、部署路径及密钥不包含在内，示例不能直接迁移生产数据。infinite-server移动参考宿主尚未接入此持久化库。
