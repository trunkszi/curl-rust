# Spec：curl-rust 独立维护分支（curl 8.22.0 / x86_64 / 单线程 / epoll / WebSocket）

Status: done
Type: spec
Created: 2026-09-14
Updated: 2026-09-14（合并访谈结论与审查意见后的第二版）

## 问题陈述

上游 curl-rust 是一个面向所有平台、允许跨线程使用、以阻塞式 `perform` 为主入口、支持二十余种协议和多种 TLS 后端的通用绑定。我们的使用场景与之相反：只跑在 x86_64 上，程序严格单线程单进程，所有 I/O 由一个 epoll 事件循环驱动，同时使用 HTTP 和 WebSocket 两种协议，并且由应用自己负责 DNS 解析。上游代码中的多平台分支、`Send`/`Sync` 声明、线程化 DNS 解析器、`Easy`/`Easy2` 双类型体系、阻塞式 `perform`、OpenSSL 依赖以及大量用不到的协议，在这个场景下不仅是死代码，还会误导使用者写出与约束冲突的代码。此外上游 vendored 的 curl 停在 8.21.0，而 8.22.0 已发布。

## 解决方案

基于当前 `main` 建立独立维护的代码线（推送到私有仓库自行维护），做七件事：

1. 把 vendored curl 升级到 8.22.0。
2. 只保留 x86_64 的三个目标平台（linux-gnu、linux-musl、apple-darwin），删除 Windows 及其它平台的全部代码、依赖、CI。
3. 协议面收窄到 HTTP(S) 与 WS(S)，TLS 后端固定为 rustls（加密提供者 aws-lc-rs），删除 OpenSSL 及其它协议、认证方式相关的代码、依赖和 Cargo 特性；zlib 内容解压与 HTTP/SOCKS 代理保留，HTTP/2 作为默认关闭的可选特性保留。
4. 连接目标完全由应用给定：URL 主机必须是 IP 字面量或已通过预置映射指定 IP，libcurl 的 DNS 解析器永远不会被触达；自动重定向、Alt-Svc、DoH 一并移除。
5. 把库改造为严格单线程：libcurl 产物中不再包含任何创建线程的代码，Rust 类型在编译期禁止跨线程移动或共享。
6. 把默认使用方式从阻塞式 `perform` 改为事件循环：所有传输挂在内部的 multi 句柄上，由内置的 epoll（Linux）/ kqueue（macOS）反应器通过 `curl_multi_socket_action` 驱动；HTTP 传输与 WebSocket 连接共用同一个事件循环。
7. 把 libcurl 的 WebSocket 能力以 pull 模式（`CONNECT_ONLY = 2`，即 `curl_ws_recv`/`curl_ws_send`）暴露到 Rust 层，对应用呈现为消息语义的 `WebSocket` 连接对象。

同时把 `Easy`/`Easy2`/`Handler`/`transfer` 合并为单一的闭包式 `Easy` 类型，`Multi` 转为内部类型，公开 API 只剩 `Easy`、`EventLoop`、`websocket` 模块和错误类型。

## 用户故事

### 版本与平台

1. 作为库的维护者，我希望 vendored curl 指向 8.22.0 且 `curl-sys` 版本号随之更新，以便获得该版本的修复并让版本号如实反映内容。
2. 作为库的维护者，我希望 curl 8.22.0 新增的必要源文件（API guards、uint-hashset）和迁移到 `vdns/` 的 DNS 源文件被正确编译，以便静态库能够链接。
3. 作为库的维护者，我希望 HTTP Message Signatures 功能在本分支默认关闭，以便不引入 ed25519 相关的额外编译面。
4. 作为库的维护者，我希望 `curl-sys` 的常量、选项、错误码与 8.22.0 的头文件一致，并由 systest 自动校验，以便绑定不会悄悄漂移。
5. 作为库的维护者，我希望 `curl-sys` 的构建脚本在非 x86_64 或非 Linux/macOS 的目标上直接报错，以便任何人都不会在不支持的平台上得到一个"看似能编译"的产物。
6. 作为库的维护者，我希望 `Cargo.toml` 中不再有 Windows 专属依赖（schannel、windows-sys、vcpkg）和 Windows/macOS 系统库相关特性，以便依赖树只包含实际使用的内容。
7. 作为库的使用者，我希望三个目标平台都统一从源码静态编译 libcurl，以便 WebSocket 等能力不依赖系统 libcurl 的编译选项。
8. 作为库的使用者，我希望 musl 目标继续可用且不依赖任何系统 TLS 库，以便生成静态链接的二进制。
9. 作为库的维护者，我希望 CI 矩阵只包含三个目标平台（Linux stable/beta/nightly、musl、macOS x86_64 使用 `macos-15-intel` 运行器），以便 CI 只验证我们承诺支持的组合。
10. 作为库的维护者，我希望工具链跟随最新 stable、edition 升到 2024、不承诺最低版本，以便不受旧编译器约束且少一个 CI 任务。
11. 作为库的维护者，我希望 README 如实描述本分支的平台范围、线程约束、DNS 策略和使用方式，以便读者不会按上游文档误用。

### TLS 与协议

12. 作为库的使用者，我希望 TLS 后端固定为 rustls 且无需任何 Cargo 特性即可启用，以便 HTTPS 和 WSS 开箱即用。
13. 作为库的使用者，我希望 rustls 使用 aws-lc-rs 作为加密提供者，以便与 rustls 上游默认一致并获得最好的维护。
14. 作为库的使用者，我希望依赖树中不再有 openssl-sys、openssl-probe 以及 OpenSSL 相关的构建脚本，以便不需要系统 OpenSSL 头文件和库。
15. 作为库的使用者，我希望 `Easy::new()` 默认启用平台证书验证器（`CURLSSLOPT_NATIVE_CA`），以便在没有配置 CA 文件时 HTTPS 连接也能正确校验证书而不是失败。
16. 作为库的使用者，我希望仍能通过 `cainfo`、`ssl_cainfo_blob`、`ssl_cert`/`ssl_key`、`ssl_verify_peer`、`ssl_min_max_version`、`ssl_cipher_list` 等方法配置 TLS，以便覆盖 rustls 后端支持的全部选项。
17. 作为库的使用者，我希望 libcurl 只编译 HTTP(S) 与 WS(S) 协议，其它协议（FTP、FILE、MQTT、SMTP、IMAP、POP3、DICT、GOPHER、TELNET、TFTP、RTSP、SMB、LDAP、SSH、HTTP/3）的源码不进入编译列表，以便产物最小、攻击面最小。
18. 作为库的使用者，我希望 `Easy` 上不再有 FTP、AWS SigV4、NTLM、SPNEGO、OpenSSL 上下文回调、自动重定向、Alt-Svc、DoH 等与本分支能力无关的方法，以便 API 面只反映实际能力。
19. 作为库的使用者，我希望 HTTP 层的能力（cookie、HSTS、multipart/mime、Basic/Digest/Bearer 认证、HTTP 代理与 CONNECT 隧道、SOCKS 代理、gzip/deflate 解压、自定义头、请求方法、超时）完整保留，以便 HTTP 客户端功能不缩水。
20. 作为库的使用者，我希望 HTTP/2 作为默认关闭的可选特性保留，以便需要时开启多路复用而不影响默认构建体积。
21. 作为库的维护者，我希望 Cargo 特性只剩 `http2` 一个，其余（`ssl`、`rustls`、`static-curl`、`static-ssl`、`mesalink`、`spnego`、`ntlm`、`protocol-ftp`、`zlib-ng-compat`、`windows-static-ssl`、`force-system-lib-on-osx`、`poll_7_68_0`、`upkeep_7_62_0`）全部删除，以便特性组合不再需要测试矩阵。

### 连接目标与 DNS

22. 作为库的使用者，我希望由应用自己解析 DNS，然后通过 `Easy::resolve_host(host, port, &[IpAddr])` 把主机名到 IP 的映射交给句柄，以便 URL 里仍写主机名而 SNI、证书校验、`Host` 头、cookie 域都保持正确。
23. 作为库的使用者，我希望 URL 主机是 IP 字面量时无需任何预置即可直接连接，以便简单场景零配置。
24. 作为库的使用者，我希望把 `Easy` 加入事件循环时，库检查 URL 主机和代理主机要么是 IP 字面量要么已有预置映射，不满足则立即返回错误，以便 libcurl 的同步解析器永远不会被调用、事件循环永远不会因 DNS 阻塞。
25. 作为库的使用者，我希望库不自动跟随重定向，而是让我从响应头读到 `Location` 后自行决定，以便不会在传输中途连向一个未预置的主机。
26. 作为库的使用者，我希望 Alt-Svc 和 DoH 从库中移除，以便不存在任何库自行发起的、目标未经应用给定的连接。

### 单线程

27. 作为库的使用者，我希望 libcurl 产物中不存在线程化 DNS 解析器和线程池代码，以便运行时不会有库自行创建的线程。
28. 作为库的使用者，我希望 `Easy`、`EventLoop`、`WebSocket`、`List`、`Form` 等类型在编译期就是 `!Send`，以便把它们放进 `std::thread::spawn` 或 `Arc` 时得到编译错误而不是运行时未定义行为。
29. 作为库的使用者，我希望所有回调只要求 `FnMut` 而不要求 `Send`，以便回调可以直接捕获 `Rc<RefCell<T>>` 共享状态。
30. 作为库的使用者，我希望句柄默认不安装信号处理器（`NOSIGNAL`），以便 libcurl 不会通过 `SIGALRM` 干扰我的事件循环。
31. 作为库的使用者，我希望 `Version` 能如实报告异步 DNS 不可用、WebSocket 可用，以便运行时自检。

### 单一 Easy 类型

32. 作为库的使用者，我希望只有一个 `Easy` 类型而不是 `Easy`/`Easy2` 两套，以便不必在两种回调机制间做选择。
33. 作为库的使用者，我希望所有 libcurl 回调（write、read、header、progress、debug、seek、open_socket）都通过闭包设置，以便用法统一。
34. 作为库的使用者，我希望 `Easy` 上的全部 HTTP 相关 setopt 方法在合并后一个不少，以便功能不回退。
35. 作为库的使用者，我希望 `Easy` 在同一个线程内可以被 `EventLoop` 接管后再交还给我，以便重用配置发起下一次传输。
36. 作为库的维护者，我希望 `transfer()` 及其借用局部变量的机制被移除，以便消除最复杂的生命周期代码；共享状态改由 `Rc<RefCell>` 表达。
37. 作为库的使用者，我希望 HTTP 侧保持 libcurl 风格的层级（逐项 setopt，响应体由 `write_function` 收集），暂不加便捷的 `Response` 类型，以便先把底层做对。

### 事件循环

38. 作为库的使用者，我希望 `Easy` 上不再有阻塞的 `perform()`，以便所有传输都必须经过事件循环。
39. 作为库的使用者，我希望有一个内置的 `EventLoop` 类型，能把 `Easy` 加入其中并返回一个 `Token`，以便后续引用这次传输。
40. 作为库的使用者，我希望在加入传输时提供一个完成回调，传输结束时在当前线程收到 `Easy` 和结果，以便处理响应并回收句柄。
41. 作为库的使用者，我希望 `EventLoop::run()` 一直跑到没有活跃传输且没有打开的 WS 连接为止，并有 `run_until(闭包)` 供长连场景自定义退出条件，以便脚本式和常驻式两种用法都能覆盖。
42. 作为库的使用者，我希望 `EventLoop::run_once(max_wait)` 只做一轮等待与分发，以便把 curl 嵌入我自己的主循环。
43. 作为库的使用者，我希望 `EventLoop` 暴露其底层 epoll/kqueue 的 fd 和 `next_timeout()`，以便把整个 curl 循环作为一个 fd 挂到我外层的 epoll 上，可读或超时时调用 `run_once(0)`。
44. 作为库的使用者，我希望在同一个 `EventLoop` 上同时跑多个 HTTP 传输和多条 WS 连接，两者共用一个 epoll 且互不阻塞，以便单线程单进程程序只有一个主循环。
45. 作为库的使用者，我希望 `EventLoop` 允许我注册自己的 fd 和事件回调，以便在同一个循环里处理其它 I/O。
46. 作为库的使用者，我希望能通过 `Token` 取消一个进行中的传输并拿回 `Easy`，以便实现超时或用户中断。
47. 作为库的使用者，我希望能通过 `Token` 临时访问进行中传输的 `Easy`（例如 `unpause`），以便在传输过程中与之交互。
48. 作为库的使用者，我希望在 `with` 闭包内做重入操作（调用 `run_once`、取消当前 token 等）时立即 panic，以便编程错误尽早暴露而不是被当作可恢复条件。
49. 作为库的使用者，我希望 curl 的定时器需求通过 `epoll_wait` 的超时参数实现而不引入 timerfd，以便实现简单且在 macOS 上同样适用。
50. 作为库的使用者，我希望 curl 报告的 socket 事件变化被正确映射为 epoll/kqueue 的注册、修改、删除，以便不会漏事件或对已关闭 fd 操作。
51. 作为 macOS 使用者，我希望同一套 `EventLoop` API 在 macOS 上由 kqueue 后端实现，以便代码无需条件编译即可在两个系统上运行。
52. 作为库的维护者，我希望 `Multi` 及其 `perform`/`wait`/`poll`/`fdset`/`MultiWaker` 等全部从公开 API 中消失，只作为 `EventLoop` 的内部实现，以便公开 API 面最小。

### WebSocket

53. 作为库的使用者，我希望 `curl-sys` 提供 `curl_ws_recv`、`curl_ws_send`、`curl_ws_meta`、`curl_ws_start_frame` 四个函数、`curl_ws_frame` 结构体、`CURLWS_*` 标志、`CURLOPT_WS_OPTIONS` 和 `CURLINFO_ACTIVESOCKET` 的绑定，以便有需要时能直接调用原始 API。
54. 作为库的使用者，我希望把一个 `ws://` 或 `wss://` 的 `Easy` 交给 `EventLoop::connect_websocket(easy, options, on_open)`，握手成功后在当前线程收到一个 `WebSocket`，握手失败则收到错误，以便建立连接与后续收发分离。
55. 作为库的使用者，我希望握手完成后 `Easy` 继续由 `EventLoop` 持有并留在内部 multi 句柄中，以便 libcurl 能找到该连接，而我不必关心句柄生命周期。
56. 作为库的使用者，我希望通过 `WebSocket::on_message(|msg| ...)` 按完整消息接收，`msg` 是 `Message::{Text, Binary, Ping, Pong, Close}`，分片由库拼接，以便应用只处理消息语义。
57. 作为库的使用者，我希望拼接中的消息有大小上限（`Options` 可调），超限时连接以错误关闭，以便内存边界可控。
58. 作为库的使用者，我希望 `WebSocket::send(Message)` 一次发送一条完整消息，libcurl 返回 `CURLE_AGAIN` 时未发出的部分由库缓存并在 socket 可写时自动续发，以便我不必处理部分发送。
59. 作为库的使用者，我希望发送缓存有上限（默认 4 MiB，`Options` 可调），超限时 `send` 返回错误、消息被拒绝而连接保持，以便应用能感知背压。
60. 作为库的使用者，我希望 `WebSocket::close(code, reason)` 发送 CLOSE 帧后由库等待对端 CLOSE，超时（默认 5 秒，`Options` 可调）则强制断开，两种情况都恰好触发一次 `on_close`，以便关闭握手符合协议且不需要我计时。
61. 作为库的使用者，我希望收到对端 CLOSE 帧或连接断开时也触发 `on_close`，以便所有结束路径统一。
62. 作为库的使用者，我希望 `Options` 提供默认关闭的 `ping_interval` 与 `idle_timeout`，开启后由事件循环定时器驱动主动 PING 和空闲断开，以便长连探活不需要每个应用重复实现。
63. 作为库的使用者，我希望 libcurl 对服务器 PING 的自动 PONG 在 pull 模式下照常工作，且 `on_message` 也能看到 PING/PONG，以便我可以选择自行处理心跳。
64. 作为库的使用者，我希望关闭或丢弃 `WebSocket` 时，`EventLoop` 先注销 socket 监听再从内部 multi 移除并释放 `Easy`，以便不会有悬空 fd 或重复所有权。
65. 作为库的使用者，我希望通过 `Easy::websocket_options` 设置 raw mode 与 no-auto-pong 等 libcurl 级选项，以便控制帧处理行为。
66. 作为库的使用者，我希望有一个 WebSocket 示例展示从建立连接、收发 text/binary 消息、响应 ping、到发送 close 的完整流程，以便照着写。
67. 作为库的使用者，我希望 `Version` 提供 `feature_websockets()`，以便运行时确认库支持 WebSocket。

### 错误与故障

68. 作为库的使用者，我希望事件循环与 WebSocket 的方法返回一个新的 `event_loop::Error` 枚举，变体包装原有的 `Error`（CURLcode）、`MultiError`、`std::io::Error`，并新增主机未预置、发送缓存超限、消息超限、关闭超时等变体，以便一处匹配所有失败原因。
69. 作为库的使用者，我希望原有的 `Error`、`MultiError`、`FormError` 类型及其判断方法（如 `is_again`）保留，以便与 CURLcode 一一对应的判断继续可用。
70. 作为库的使用者，我希望用户回调里的 panic 被捕获并在本轮 libcurl 调用返回后重新抛出，以便 panic 不会穿过 C 栈帧，同时也不会被掩盖成错误。

### 质量

71. 作为库的维护者，我希望所有现有 HTTP 测试在改造后仍然通过（`perform` 调用改写为 `EventLoop::run`），以便功能不回退。
72. 作为库的维护者，我希望有一个用 Python 标准库手写的本地测试服务（单文件，无 pip 依赖），提供 WebSocket echo（握手、掩码帧、分片、ping/pong、close）以及基于自签名证书的 WSS 和最小 HTTPS 端点，由 Rust 测试以子进程方式启动，以便 WebSocket 与 TLS 路径有可重复的回归测试且对端实现与被测代码完全独立。
73. 作为库的维护者，我希望 systest 在三个平台上都能对照头文件校验绑定，以便结构体布局和常量值正确。
74. 作为库的维护者，我希望单线程约束有三重验收：构建列表断言无线程源文件、产物符号扫描无线程创建函数、运行时线程数回归测试，以便任何一层失效都能被发现。
75. 作为库的维护者，我希望每一步改造都是一个独立提交并附带测试结果，以便逐步审查和回退。

## 实现决策

### 分支与版本

- 基于当前 `main`（提交 `bae5dc5`）改造，推送到私有仓库维护，不追求与上游 API 兼容，不向上游回推。`.scratch/` 与 `docs/agents/` 随仓库提交作为设计记录。
- `curl-sys` 版本改为 `0.4.91+curl-8.22.0`；`curl` crate 升到 `0.5.0` 以表明 API 不兼容。crate 名不变。
- edition 升到 2024，工具链跟随最新 stable，不承诺最低版本。

### curl 8.22.0 升级

- 子模块指向标签 `curl-8_22_0`。实施时校验标签、头文件符号与 `curl_version_info` 报告的版本一致，systest 通过后才更新版本号。
- 编译列表调整：`asyn-base`、`cf-dns`、`dnscache`、`hostip`、`hostip6` 改到 `vdns/` 目录；新增 `api.c` 与 `uint-hashset.c`；`libcurl.pc` 的版本串同步。
- 定义 `CURL_DISABLE_HTTPSIG`，不编译 `http_httpsig.c` 与 `curl_ed25519.c`。
- `curl-sys` 补充 8.22.0 的符号：`CURLAUTH_HTTPSIG` 及其对 `CURLAUTH_ANY`/`CURLAUTH_ANYSAFE` 取值的影响、`CURLOPT_HTTPSIG_*` 四个选项、`CURLUE_BACKSLASH`。`CURLOPT_TLSAUTH_*` 已废弃，绑定保留但 systest 按版本跳过。

### 平台裁剪

- 支持的目标三元组固定为 `x86_64-unknown-linux-gnu`、`x86_64-unknown-linux-musl`、`x86_64-apple-darwin`。构建脚本对 `TARGET` 做白名单检查，不在其中则 `panic` 并给出明确信息；`curl` crate 顶层同时用 `compile_error!` 做第二道守卫。
- 构建脚本删除 windows、solaris 分支与 vcpkg；保留 Apple 分支的宏定义与 framework 链接；`SIZEOF_*` 系列直接写死 64 位值。
- 删除 pkg-config 与 macOS 系统 libcurl 探测；从源码静态编译是唯一路径。
- `curl-sys` 中 `fd_set`、`curl_socket_t`、`__enum_ty` 只保留 unix 定义。
- `curl` crate 删除 Windows 证书注入模块；`open_socket`、`setopt_path`、`path2cstr` 只保留 unix 实现；构造函数的 `link_section` 只保留 linux 与 macos。
- CI 矩阵缩减为 linux64 stable/beta/nightly、musl、macOS x86_64（`macos-15-intel` 运行器，该运行器到 2027 年 8 月为止可用，到期后改为 arm64 运行器交叉编译加 Rosetta 运行测试）；删除 mingw、linux32、win32、win64 及对应 Dockerfile 和 1.85 版本任务。

### TLS 与协议裁剪

- TLS 后端固定为 rustls：`rustls-ffi` 成为 `curl-sys` 的必选依赖，加密提供者为 aws-lc-rs（rustls-ffi 默认）。若 musl 目标编译 aws-lc-sys 遇到障碍，退路是切换到 `ring` 提供者；两者都不创建线程，对 Rust 层 API 无影响。
- 构建脚本无条件定义 `USE_RUSTLS` 并编译 rustls 后端及 cipher_suite 源文件。删除 `USE_OPENSSL` 分支、`openssl-sys`、`openssl-probe`、`static-ssl`、`curl` crate 顶层的 OpenSSL 初始化构建脚本以及初始化函数中的 OpenSSL 探测调用。
- 证书校验默认值：`Easy::new()` 默认设置 `CURLSSLOPT_NATIVE_CA`，使 rustls 使用平台验证器。原因是 curl 的 rustls 后端在开启校验但未提供 CA 文件/blob 且未启用平台验证器时没有可用的验证器，连接会直接失败；vendored 编译也没有内置 CA 路径。使用者设置 `cainfo` 或 `ssl_cainfo_blob` 后按 curl 的既定优先级覆盖。
- rustls 后端不支持 `CURLOPT_SSL_CTX_FUNCTION`，删除 `ssl_ctx_function` 回调及相关类型；CRL 文件与平台验证器不可同时使用，文档中说明。
- 协议面只保留 HTTP、HTTPS、WS、WSS。构建脚本在现有 `CURL_DISABLE_*` 之外增加 FILE、MQTT、FTP、AWS、NETRC、SMB、ALTSVC、DOH 的禁用宏，并把顶层守卫在这些宏下为空的源文件（各协议实现、fileinfo、curl_range、http_aws_sigv4、altsvc、vdns/doh、vssh、vquic、OpenSSL 专属的 hostcheck/keylog 等）从编译列表删除。HTTP 相关能力全部保留：cookie、HSTS、mime/formdata、Basic/Digest/Bearer 认证、HTTP 代理与 CONNECT 隧道、SOCKS 代理、内容解压。
- zlib 保留：`libz-sys` 继续作为必选依赖用于 gzip/deflate 解压，删除 `zlib-ng-compat` 特性。
- HTTP/2 保留为默认关闭的可选特性 `http2`，开启时链接 `libnghttp2-sys`。
- 删除的 Cargo 特性：`ssl`、`rustls`、`static-curl`、`static-ssl`、`mesalink`、`spnego`、`ntlm`、`protocol-ftp`、`zlib-ng-compat`、`windows-static-ssl`、`force-system-lib-on-osx`、`poll_7_68_0`、`upkeep_7_62_0`。
- `Easy` 删除与被裁协议、后端或策略绑定的方法：FTP 系列选项、`aws_sigv4`、NTLM/SPNEGO 相关认证位、`ssl_ctx_function`、`upkeep`、`follow_location` 及重定向相关选项、Alt-Svc 与 DoH 选项。`Auth` 类型只保留 Basic、Digest、Bearer。

### 连接目标与 DNS

- 库不做 DNS 解析，也不让 libcurl 做。依据是 libcurl 的两条免解析路径：URL 主机为 IP 字面量时直接构造地址；通过 `CURLOPT_RESOLVE` 预置的 `host:port:ip` 条目是永久缓存项，命中后不调用 `getaddrinfo`。
- `Easy::resolve_host(host, port, &[IpAddr])` 是预置映射的类型化接口，内部生成 `CURLOPT_RESOLVE` 条目，同时在 Rust 侧记录（主机, 端口）集合供校验用。原来接受字符串列表的 `resolve` 方法删除。
- 校验发生在 `EventLoop::add` 与 `connect_websocket`：用 curl 的 URL 解析器取出 URL 的主机与端口，若主机既不是 IP 字面量、`(主机, 端口)` 又不在预置集合中，返回 `event_loop::Error::UnresolvedHost`。若设置了代理，对代理主机与端口做同样校验。校验失败时 `Easy` 原样交还。
- 自动重定向、Alt-Svc、DoH 移除后，libcurl 在一次传输中只会连接应用给定的目标。应用需要重定向时从响应头读 `Location`，自行解析后发起新传输。
- 同步解析器的源码（`vdns/hostip*.c`）仍在编译列表中，因为 libcurl 的地址构造路径依赖其中的公共函数；上述校验保证 `getaddrinfo` 分支永远不被执行。运行时线程数回归测试与"URL 主机未预置必须报错"的测试共同守住这一点。

### 单线程

- 构建脚本删除 `USE_RESOLV_THREADED`、`HAVE_PTHREAD_H`、`HAVE_THREADS_POSIX`，curl 落到 `CURLRES_SYNCH`。删除 `curl_threads.c`、`thrdpool.c`、`thrdqueue.c`、`vdns/asyn-thrdd.c`。构建脚本中加一条断言：编译列表里不允许出现这四个文件名。
- 删除全部 `unsafe impl Send`/`unsafe impl Sync`（`Easy` 内部数据、`List`、`MultiWaker`、`Version`）。
- `Easy`、`EventLoop`、`WebSocket`、`Token`、`List`、`Form` 加入 `PhantomData<*mut ()>` 标记字段，使 `!Send`/`!Sync` 成为显式契约。这只约束 Rust 侧，libcurl 产物不创建线程由构建断言、符号扫描和运行时测试三重验收保证。
- 内部 multi 句柄的引用计数用 `Rc`；所有回调类型去掉 `Send` 约束。
- `Easy::new()` 默认设置 `CURLOPT_NOSIGNAL`。
- 测试用的 Python 服务以子进程运行，属于测试基础设施；库代码本身不得引用 `std::thread`，现有基于线程的 Rust mock HTTP 服务在改写测试时一并迁移到 Python 服务或保留在测试代码内，不进入库产物。

### 单一 Easy

- 删除 `Easy2<H>`、`Handler` trait、`Easy2Handle`、`transfer()` 与 `Transfer` 类型。
- `Easy` 为非泛型类型，内部持有一组 `Box<dyn FnMut>` 回调；所有 HTTP 相关 setopt 方法从原 `Easy2` 实现迁入。
- 回调设置方法保持现有闭包签名：`write_function`、`read_function`、`header_function`、`progress_function`、`debug_function`、`seek_function`、`open_socket_function`。WebSocket 不经过这些回调。
- 用户回调中的 panic 沿用现有捕获机制：捕获后在本轮 libcurl 调用返回时重新抛出，`run_once` 因 panic 退出，`EventLoop` 之后不可再用。

### 事件循环

- `EventLoop` 内部包含：一个内部 multi 句柄、一个反应器、fd 归属表、由 curl timer 回调维护的"下次超时"、`Token` 到传输记录（`Easy` 与完成回调）的映射、WebSocket 连接表、使用者自注册 fd 的表、定时器堆（WebSocket 的 ping/idle/close 超时与用户定时器）。
- 反应器是内部模块，Linux 后端基于 `libc` 的 epoll，macOS 后端基于 `libc` 的 kqueue；不引入 mio。对外接口只有注册、修改、注销、等待四个操作，两个后端行为一致。
- fd 归属表是 HTTP 与 WebSocket 共存的核心：每个注册到反应器的 fd 有且只有一个归属，取值为 Curl（由 libcurl 的 socket 回调管理，用于 HTTP 传输和 WebSocket 握手）、Ws(token)（握手完成后由 `EventLoop` 直接管理的 WebSocket socket）或 User(token)（使用者通过 `watch` 注册）。就绪事件按归属分发：Curl 归属调 `curl_multi_socket_action`；Ws 归属可读时循环 `curl_ws_recv`、可写时刷发送队列；User 归属调用户回调。libcurl 的 socket 回调只允许修改归属为 Curl 的 fd，对其它归属的 fd 发来的 REMOVE/修改一律忽略。
- 事件流：curl 的 socket 回调映射为反应器的注册/修改/注销；反应器返回的就绪事件映射为 `CURL_CSELECT_IN`/`OUT`/`ERR` 调用 `curl_multi_socket_action`；等待超时后以 `CURL_SOCKET_TIMEOUT` 调用并触发到期的定时器；每次 socket_action 之后读取 `curl_multi_info_read`，普通传输 DONE 时调用完成回调并交还 `Easy`，`CONNECT_ONLY = 2` 的传输 DONE 时进入 WebSocket 的握手完成流程。
- 等待时长为 curl timer、定时器堆最近到期时间、`run_once` 的 `max_wait` 三者最小值。`next_timeout()` 对外暴露。
- 运行方式：`run_once(max_wait)` 做一轮，返回是否仍有活跃传输或连接；`run_until(闭包)` 循环到闭包返回真；`run()` 等价于"传输数与打开的 WebSocket 连接数都为零时退出"。已有自己主循环的使用者把 `as_raw_fd()` 挂到外层 epoll，可读或 `next_timeout()` 到期时调 `run_once(0)`。文档明确：反应器 fd 可读不是唯一驱动条件，外层循环必须同时遵守 `next_timeout()`。
- `EventLoop` 公开接口：`new()`、`add(easy, on_done) -> Token`、`cancel(token) -> Easy`、`with(token, |easy| ...)`、`connect_websocket(easy, options, on_open)`、`run()`、`run_until(闭包)`、`run_once(max_wait)`、`next_timeout()`、`as_raw_fd()`、`watch(fd, events, callback) -> WatchToken`、`unwatch(token)`、`add_timer(at, callback)`，以及连接数上限等少量 multi 级设置的转发方法。
- 重入保护：`with` 闭包内调用 `run_once`、`cancel` 当前 token 或其它会执行完成回调的操作时 panic。`watch` 回调不持有 `EventLoop` 的可变借用，也不能在回调中直接注销自身；事件先收集到本轮队列再依次分发，`unwatch` 后保证不再投递旧事件。
- `Easy` 删除 `perform()`。`Multi` 及 `MultiError` 之外的相关公开类型（`EasyHandle`、`WaitFd`、`MultiWaker`、`Events`、`SocketEvents`、`Message`）全部转为 crate 内部或删除。

### WebSocket

- 只采用 pull 模式：`Easy` 设置 `CONNECT_ONLY = 2`，握手由内部 multi 通过 socket_action 驱动，握手完成表现为 `curl_multi_info_read` 报告该传输 DONE。不采用 write/read 回调模式，理由：回调内调用 `curl_ws_send` 是阻塞发送；回调模式下唯一的非阻塞发送路径需要 read 回调配合 `CURLOPT_UPLOAD` 与 pause/unpause，且 `curl_ws_recv` 在该模式下不可用。
- 握手后 `Easy` 必须留在内部 multi 中：curl 通过 `lastconnect_id` 在 multi 的连接池里查找 connect_only 连接，移出后 `curl_ws_recv`/`curl_ws_send` 找不到连接。`EventLoop` 持有该 `Easy` 直到 `WebSocket` 关闭或丢弃。
- 握手完成时的归属切换：DONE 后用 `CURLINFO_ACTIVESOCKET` 取 fd，把归属从 Curl 改为 Ws(token) 并把监听改为可读。libcurl 对该 fd 的 `CURL_POLL_REMOVE` 可能先于或后于 DONE 到达，两种顺序都必须正确（先到则 DONE 时新增注册，后到则忽略）。握手失败时 `on_open` 收到错误并释放 `Easy`。
- `curl-sys` 新增：`curl_ws_frame` 结构体、`CURLWS_*` 标志、`CURLWS_RAW_MODE`/`CURLWS_NOAUTOPONG` 选项位、`CURLOPT_WS_OPTIONS`、`CURLINFO_ACTIVESOCKET`，以及 `curl_ws_recv`、`curl_ws_send`、`curl_ws_meta`、`curl_ws_start_frame` 四个外部函数（后两个仅作为原始绑定提供，封装层不使用）。
- `curl` crate 新增 `websocket` 模块，包含 `WebSocket`（连接对象）、`Message`（`Text(String)`、`Binary(Vec<u8>)`、`Ping(Vec<u8>)`、`Pong(Vec<u8>)`、`Close { code, reason }`）、`Options`、`CloseCode`。`Frame`/`FrameFlags` 作为内部类型存在，不公开。`Easy` 只新增 `websocket_options` 与 `resolve_host`。
- 接收：反应器报告可读时，`EventLoop` 循环调用 `curl_ws_recv` 直到 `CURLE_AGAIN`，按帧元数据把片段拼接成消息：TEXT/BINARY 首帧开始新消息，CONT 帧追加，`bytesleft` 为零且非 CONT 的最后一帧结束消息并触发 `on_message`；控制帧不可分片，直接投递。TEXT 消息在投递前做 UTF-8 校验，失败按协议以错误关闭。拼接中的消息超过 `Options::max_message_size`（默认值实施时定）时以错误关闭连接。
- 发送：`send(Message)` 把消息编码为一个帧交给 `curl_ws_send`；返回 `CURLE_AGAIN` 时把本次调用未发出的尾部放入连接的待发送队列，并把反应器监听改为可读加可写；可写事件到来时先刷队列，队列清空后取消可写监听。队列上限默认 4 MiB，`Options::max_send_buffer` 可调，超限时 `send` 返回 `event_loop::Error::SendBufferFull`，消息被拒绝，连接保持。
- 关闭：`close(code, reason)` 发送 CLOSE 帧并启动定时器（`Options::close_timeout`，默认 5 秒）；收到对端 CLOSE 或超时后断开，恰好触发一次 `on_close`。收到对端主动 CLOSE 时回 CLOSE 后断开；`curl_ws_recv` 报告连接断开时直接进入关闭。所有路径统一为：注销反应器监听、从内部 multi 移除、释放 `Easy`、触发 `on_close`。
- 探活：`Options::ping_interval` 与 `Options::idle_timeout` 默认关闭。开启后由定时器堆驱动：间隔到期发送 PING；`idle_timeout` 内没有收到任何帧则以错误关闭。libcurl 的自动 PONG 保持默认开启；`on_message` 仍能看到 PING/PONG。
- `Version` 新增 `feature_websockets()`，基于 feature_names 判断。

### 错误

- 新增 `event_loop::Error` 枚举，变体包装 `Error`（CURLcode）、`MultiError`、`std::io::Error`，并新增 `UnresolvedHost`、`SendBufferFull`、`MessageTooLarge`、`CloseTimeout`、`InvalidUtf8`、`HandshakeFailed` 等。`EventLoop` 与 `WebSocket` 的方法统一返回它。
- 原有 `Error`、`MultiError`、`FormError` 保留，`Easy` 的 setopt 方法继续返回 `Error`。

### 公开 API 面

- `curl` crate 公开的类型：`Easy` 及其配置用的辅助类型（`List`、`Form`、`Auth`、`SslOpt`、`ProxyType` 等）、`EventLoop`、`Token`、`WatchToken`、`websocket::{WebSocket, Message, Options, CloseCode}`、`event_loop::Error`、原有 `Error`/`MultiError`/`FormError`、`Version`。
- `Multi` 及其配套类型不公开。

## 测试决策

### 什么是好的测试

只测试外部可观察行为：请求是否按预期到达测试服务、响应内容是否正确送到回调、WebSocket 消息是否按类型和内容往返、错误是否以正确的变体返回、类型是否在编译期拒绝跨线程、进程是否只有一个线程。不测试反应器内部注册表、curl 回调调用次数等实现细节。

### 测试接缝

1. **`EventLoop` + 本地测试服务**（主接缝，替代原来的 `perform` + mock 服务）。现有 HTTP 测试都通过本地 mock 服务验证行为，改造后同一批测试改为把 `Easy` 交给 `EventLoop::run()`，断言保持不变。这是覆盖"合并 Easy"、"删除 perform"、"事件循环"、"DNS 校验"四项改动的唯一主接缝。
2. **Python 测试服务**：单文件、只用标准库、放在测试目录下；提供明文 WebSocket echo（握手、掩码帧解析、分片回显、PING/PONG、CLOSE 握手、可按指令制造慢读以触发发送背压）、WSS、最小 HTTPS 端点。TLS 用提交到仓库的自签名证书与私钥（长有效期）。Rust 测试以子进程启动它、从其标准输出读取实际监听端口、结束时终止进程。`python3` 不存在时测试失败而不是跳过。
3. **systest**：现有的 ctest 对照头文件校验 `curl-sys` 的结构体与常量，覆盖 8.22.0 符号与 WebSocket 绑定。
4. **编译期负向测试**：用 doctest 的 `compile_fail` 断言 `Easy`、`EventLoop`、`WebSocket` 不能被 `thread::spawn` 移动、不能放入 `Arc` 后跨线程共享。
5. **单线程三重验收**：构建脚本对编译列表做断言（无线程源文件）；CI 中对产物做符号扫描（`pthread_create` 等不得出现在 libcurl 静态库的已定义或未定义符号中）；运行时测试在跑完 HTTP 与 WebSocket 全流程后读取进程线程数（Linux 读 `/proc/self/status`，macOS 用等价系统调用）断言为 1。
6. **DNS 策略测试**：URL 主机为域名且未预置时 `add`/`connect_websocket` 返回 `UnresolvedHost` 且不发起任何连接；预置后能连通；IP 字面量直接连通；代理主机同样校验。

### 先例

- HTTP 行为测试：现有 `tests` 目录下基于本地 mock 服务的 `receive`/`send` 脚本式断言。
- 事件驱动测试：现有基于 mio 的 socket_action 用例，改造后由 `EventLoop` 替代 mio，逻辑等价。
- 绑定校验：`systest`。

### 测试范围

- `curl-sys`：systest 校验；构建脚本断言；符号扫描。
- `curl`：`Easy` 全部保留的 setopt 与回调、`EventLoop` 的 run/run_until/run_once/cancel/with/watch/add_timer、DNS 校验、WebSocket 的建连/收发/分片拼接/背压/关闭握手/超时/探活、TLS（HTTPS 与 WSS 经自签名证书）、`Version` 特性报告、`!Send` 编译期断言、线程数断言。
- 三个平台在 CI 中都跑完整测试；macOS 使用 `macos-15-intel` 原生运行，验证 kqueue 后端与 epoll 后端行为一致。

## 范围之外

- 不支持 x86_64 以外的架构，不支持 Windows、Solaris、BSD、Android 等系统。
- 不支持系统 libcurl（pkg-config、macOS 自带库），不做动态链接。
- 不提供任何跨线程能力：没有 `Send`/`Sync` 句柄、没有 `MultiWaker`、没有线程化或 c-ares 的异步 DNS。
- 不做 DNS 解析：没有同步解析路径的暴露、没有 DoH、没有 Alt-Svc、没有自动重定向。
- 不提供阻塞式 `perform`、`curl_multi_perform`/`wait`/`poll` 驱动方式；不公开 `Multi`。
- 不引入 mio、tokio 等运行时或反应器依赖；测试不引入 tungstenite 等 WebSocket 实现。
- 不启用 HTTP Message Signatures、HTTP/3、TLS-SRP。
- 不支持 HTTP(S)/WS(S) 以外的任何协议，不支持 OpenSSL 或其它 TLS 后端，不支持 NTLM、SPNEGO、AWS SigV4 认证。
- 不支持 WebSocket 的 write/read 回调模式，不公开帧级 API（`curl_ws_meta`、`curl_ws_start_frame` 仅保留原始绑定）。
- WebSocket 不做自动重连、子协议协商、扩展（permessage-deflate 等）。
- HTTP 侧不提供 `Response` 之类的便捷层。
- 不保证与上游 curl-rust 的 API 兼容，不向上游回推。

## 补充说明

- 已确认的决定：macOS 使用 kqueue 后端；HTTP/2 保留为默认关闭的可选特性；类型命名采用通行方案，事件循环为 `EventLoop`，WebSocket 连接对象为 `WebSocket`，消息、选项、关闭码放在 `websocket` 模块下命名为 `Message`、`Options`、`CloseCode`，由模块路径消歧而不加前缀；`with` 重入 panic；回调 panic 捕获后重新抛出；错误采用新增枚举包装旧类型；`Multi` 不公开；测试服务用 Python 标准库手写；CI 的 macOS 用 `macos-15-intel`；工具链跟随最新 stable；在私有仓库维护。
- 同步解析器在源码里仍然存在但不可达，这是本方案唯一"靠校验而非靠编译"守住的约束，因此测试接缝 5 与 6 都是必须项。
- 实施顺序建议为七个独立提交：升级 8.22.0 → 平台裁剪 → TLS 切换到 rustls 与协议/特性裁剪 → 合并 Easy 与单线程改造 → 删除 perform、实现反应器与 `EventLoop`、DNS 校验、Python 测试服务、改写测试 → WebSocket → CI/README/版本号/edition。每步都要求 `cargo test`（默认特性）与 systest 通过。
- 8.22.0 相较 8.21.0 移除了 TLS-SRP，并把 CURLAUTH_ANY 的取值再次改变，systest 的版本条件需要同步。
- curl 子模块目前在本地是浅克隆，实施时需要 `git submodule update` 到 8.22.0 提交。
- `macos-15-intel` 运行器到 2027 年 8 月为止可用，到期前需把 macOS CI 切换为 arm64 运行器交叉编译加 Rosetta 运行测试。

## Comments

- PM 决定：由于本分支定义 `CURL_DISABLE_HTTPSIG`，不绑定 `CURLOPT_HTTPSIG_*`、`CURLHTTPSIG_*` 常量及 `CURLUE_BACKSLASH`。
- PM 决定：`openssl-probe` 允许作为 rustls-platform-verifier/rustls-native-certs 的纯 Rust 传递依赖；步骤 3 的 cargo tree 验收仅要求不存在 `openssl-sys`。
- PM 决定（2026-09-14 追加）：步骤 6 必须提供 `examples/binance_ws.rs`，通过 `wss://stream.binance.com:9443/stream?streams=btcusdt@bookTicker/btcusdt@trade/btcusdt@depth5@100ms` 订阅 BBO、成交与五档深度并打印消息；示例中 DNS 由应用层用 `std::net::ToSocketAddrs` 解析后经 `resolve_host` 预置，以演示 DNS 策略。步骤 7 的最终验收增加"本机真实请求验证"：HTTP 示例访问 `https://api.binance.com/api/v3/depth?symbol=BTCUSDT&limit=5` 成功返回，WebSocket 示例在 10 秒内收到三类流的消息。本机已确认可达（HTTP 200、WS 101）。
- PM 决定（步骤 5 验收中发现）：curl 的 rustls 后端在 `CURLSSLOPT_NATIVE_CA` 开启时优先使用平台验证器并忽略 `cainfo`/`ssl_cainfo_blob`。因此 `Easy` 需跟踪当前 SSL 选项位：`cainfo`、`capath`、`ssl_cainfo_blob` 任一被设置时自动清除 `NATIVE_CA` 位（保留其它位）；使用者显式调用 `ssl_options` 则以其值为准。文档在这三个方法上注明。


## Comments

2026-09-14：步骤 5、6、7 实施完成；最终提交范围见 git log。
- PM 最终验收（2026-09-14）：全部 7 步在提交 4523b5b 验收通过。本机证据：cargo fmt --check 通过；cargo build --all-targets 零警告；cargo test 八组 0 failed；systest PASSED 3136；libcurl.a 无 pthread_create/thrd_create 符号；examples/https 返回 Binance depth 响应体；examples/binance_ws 收到 bookTicker/trade/depth5 三类消息；12 个 WebSocket 用例经检查脚本确认均有自有断言。edition 决定：curl 2024，curl-sys 与 systest 保持 2021（ctest2 解析器不支持 unsafe extern）。

## 步骤 8 追加决策：句柄与职责分离（2026-09-15）

以下决策取代用户故事 39、54 中"通过 EventLoop 直接拿到传输/连接"的表述，其余故事不变。

- `EventLoop` 协议无关：只驱动 curl multi 传输、按 fd 归属表分发就绪事件、维护定时器堆。`event_loop` 模块不得引用 `websocket` 模块的任何类型。它对外提供两类原语：`add(easy, on_done)` 与 `add_connect_only(easy, on_ready)`；后者在握手型传输 DONE 且成功时交出 `EasyHandle` 与 fd，并允许注册一个 `Driver` 接管该 fd。
- 内部 `Driver` trait：`on_readable`、`on_writable`、`on_timer`、`next_deadline`、`wants_write`，返回 `Continue | Close`。`DriverCx` 提供该连接的裸 `CURL` 指针（句柄留在 multi）与修改监听兴趣的能力。fd 归属表取值 `Curl | Driver(id) | User(token)`。
- `HttpClient`：可多实例的 HTTP 侧句柄，持有默认项（预置 DNS 映射、TLS 配置、代理、超时、公共请求头），`send(easy, on_done) -> RequestHandle`，发送时把默认项套到 `Easy` 上再交给循环；不拥有连接（连接池在 multi 中，同一循环上的客户端共享）。`RequestHandle` 提供 `cancel`、`with`。`HttpClient` drop 不影响在途请求。
- `WebSocketClient`：每条连接一个句柄，`WebSocketClient::connect(&loop, url, options, handler) -> Result<WebSocketClient>` 立即返回（状态 Connecting），可直接放入集合管理；`send` 在 Connecting 时入队、Open 后发出；事件通过应用实现的 `WebSocketHandler` trait 送达（`on_open`、`on_message`、`on_close`、`on_error`）。协议状态机（帧解析、拼接、UTF-8 校验、控制帧、发送队列、关闭握手、ping/idle）全部在 `websocket::Connection` 中，它实现 `Driver`。`WebSocketClient` drop 触发优雅关闭（语义不变）。`EventLoop::connect_websocket` 保留为一行转发以兼容。
- 三种句柄均 `!Send`，持有对循环内部状态的 `Weak` 引用；循环已销毁时方法返回 `Error::LoopGone`。所有回调在 `run_once` 分发阶段、不持有 `RefCell` 借用时调用，允许在回调内调用 `send`/`close`/`http.send`。
- 协议状态机可用内存中的假收发源做单元测试；端到端测试保留现有 12 个 WebSocket 用例与 Binance 示例作为回归网。
- 审查补充（2026-09-15，PM）：`WebSocketClient::connect` 只保留接收 `Easy` 的一个入口（URL、`resolve_host`、`cainfo` 等都在 `Easy` 上设置），不再单独提供 `connect_with`；`EventLoop::connect_websocket` 兼容转发不再保留。`DriverCx` 只提供裸 `CURL` 指针，监听兴趣由 `wants_write` 在每次分发后重新登记，不提供 `set_interest`。连接级协议错误（帧过大、UTF-8 非法、对端关闭）经 `WebSocketHandler::on_error`/`on_close` 送达对应连接，`run_once` 只对循环自身的错误返回 `Err`。多个 `WebSocketClient` 克隆共享一条连接，最后一个克隆 drop 时才发起优雅关闭；对端发起关闭时本端回应 Close 帧后再关闭。
