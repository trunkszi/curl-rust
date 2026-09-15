use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=curl");
    println!(
        "cargo:rustc-check-cfg=cfg(\
            libcurl_vendored,\
            link_libnghttp2,\
            link_libz,\
        )"
    );
    let target = env::var("TARGET").unwrap();

    const SUPPORTED: &[&str] = &[
        "x86_64-unknown-linux-gnu",
        "x86_64-unknown-linux-musl",
        "x86_64-apple-darwin",
    ];
    if !SUPPORTED.contains(&target.as_str()) {
        panic!(
            "unsupported target {}; supported targets are {:?}",
            target, SUPPORTED
        );
    }

    if !Path::new("curl/.git").exists() {
        let _ = Command::new("git")
            .args(&["submodule", "update", "--init", "curl"])
            .status();
    }

    let dst = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let include = dst.join("include");
    let build = dst.join("build");
    println!("cargo:root={}", dst.display());
    println!("cargo:include={}", include.display());
    println!("cargo:static=1");
    println!("cargo:rustc-cfg=libcurl_vendored");
    fs::create_dir_all(include.join("curl")).unwrap();

    for header in [
        "curl.h",
        "curlver.h",
        "easy.h",
        "options.h",
        "header.h",
        "mprintf.h",
        "multi.h",
        "stdcheaders.h",
        "system.h",
        "urlapi.h",
        "typecheck-gcc.h",
        "websockets.h",
    ]
    .iter()
    {
        fs::copy(
            format!("curl/include/curl/{}", header),
            include.join("curl").join(header),
        )
        .unwrap();
    }

    let pkgconfig = dst.join("lib/pkgconfig");
    fs::create_dir_all(&pkgconfig).unwrap();
    let contents = fs::read_to_string("curl/libcurl.pc.in").unwrap();
    fs::write(
        pkgconfig.join("libcurl.pc"),
        contents
            .replace("@prefix@", dst.to_str().unwrap())
            .replace("@exec_prefix@", "")
            .replace("@libdir@", dst.join("lib").to_str().unwrap())
            .replace("@includedir@", include.to_str().unwrap())
            .replace("@CPPFLAG_CURL_STATICLIB@", "-DCURL_STATICLIB")
            .replace("@LIBCURL_LIBS@", "")
            .replace("@SUPPORT_FEATURES@", "")
            .replace("@SUPPORT_PROTOCOLS@", "")
            .replace("@CURLVERSION@", "8.22.0"),
    )
    .unwrap();

    let mut cfg = cc::Build::new();
    cfg.out_dir(&build)
        .include("curl/lib")
        .include("curl/include")
        .define("BUILDING_LIBCURL", None)
        .define("CURL_DISABLE_DICT", None)
        .define("CURL_DISABLE_GOPHER", None)
        .define("CURL_DISABLE_IMAP", None)
        .define("CURL_DISABLE_LDAP", None)
        .define("CURL_DISABLE_LDAPS", None)
        .define("CURL_DISABLE_POP3", None)
        .define("CURL_DISABLE_RTSP", None)
        .define("CURL_DISABLE_SMB", None)
        .define("CURL_DISABLE_SMTP", None)
        .define("CURL_DISABLE_TELNET", None)
        .define("CURL_DISABLE_TFTP", None)
        .define("CURL_STATICLIB", None)
        .define("ENABLE_IPV6", None)
        .define("HAVE_SOCKADDR_IN6_SIN6_SCOPE_ID", None)
        .define("HAVE_ASSERT_H", None)
        .define("CURL_OS", "\"unknown\"") // TODO
        .define("HAVE_ZLIB_H", None)
        .define("HAVE_LONGLONG", None)
        .define("HAVE_LIBZ", None)
        .define("HAVE_BOOL_T", None)
        .define("HAVE_STDBOOL_H", None)
        .define("HAVE_STDINT_H", None)
        .define("CURL_DISABLE_HTTPSIG", None)
        .define("CURL_DISABLE_AWS", None)
        .define("CURL_DISABLE_ALTSVC", None)
        .define("CURL_DISABLE_DOH", None)
        .define("CURL_DISABLE_FTP", None)
        .define("CURL_DISABLE_FILE", None)
        .define("CURL_DISABLE_MQTT", None)
        .define("CURL_DISABLE_NETRC", None)
        .define("CURL_DISABLE_MQTT", None)
        .define("HAVE_GETADDRINFO", None)
        .define("HAVE_GETPEERNAME", None)
        .define("HAVE_GETSOCKNAME", None)
        .warnings(false);

    let mut sources = vec![
        "curl/lib/vtls/keylog.c",
        "curl/lib/vquic/vquic.c",
        "curl/lib/vquic/vquic-tls.c",
        "curl/lib/api.c",
        "curl/lib/uint-hashset.c",
        "curl/lib/vdns/asyn-base.c",
        "curl/lib/bufq.c",
        "curl/lib/bufref.c",
        "curl/lib/vdns/cf-dns.c",
        "curl/lib/cf-h1-proxy.c",
        "curl/lib/cf-haproxy.c",
        "curl/lib/cf-https-connect.c",
        "curl/lib/cf-ip-happy.c",
        "curl/lib/cf-recvbuf.c",
        "curl/lib/cf-setup.c",
        "curl/lib/cf-socket.c",
        "curl/lib/cfilters.c",
        "curl/lib/conncache.c",
        "curl/lib/connect.c",
        "curl/lib/content_encoding.c",
        "curl/lib/cookie.c",
        "curl/lib/creds.c",
        "curl/lib/cshutdn.c",
        "curl/lib/curl_addrinfo.c",
        "curl/lib/curl_fopen.c",
        "curl/lib/curl_get_line.c",
        "curl/lib/curl_memrchr.c",
        "curl/lib/curl_sha512_256.c",
        "curl/lib/curl_share.c",
        "curl/lib/curl_trc.c",
        "curl/lib/curlx/base64.c",
        "curl/lib/curlx/basename.c",
        "curl/lib/curlx/dynbuf.c",
        "curl/lib/curlx/fopen.c",
        "curl/lib/curlx/inet_ntop.c",
        "curl/lib/curlx/inet_pton.c",
        "curl/lib/curlx/nonblock.c",
        "curl/lib/curlx/snprintf.c",
        "curl/lib/curlx/strcopy.c",
        "curl/lib/curlx/strdup.c",
        "curl/lib/curlx/strerr.c",
        "curl/lib/curlx/strparse.c",
        "curl/lib/curlx/timediff.c",
        "curl/lib/curlx/timeval.c",
        "curl/lib/curlx/wait.c",
        "curl/lib/curlx/warnless.c",
        "curl/lib/cw-out.c",
        "curl/lib/cw-pause.c",
        "curl/lib/vdns/dnscache.c",
        "curl/lib/dynhds.c",
        "curl/lib/easy.c",
        "curl/lib/escape.c",
        "curl/lib/formdata.c",
        "curl/lib/getenv.c",
        "curl/lib/getinfo.c",
        "curl/lib/hash.c",
        "curl/lib/headers.c",
        "curl/lib/hmac.c",
        "curl/lib/vdns/hostip.c",
        "curl/lib/vdns/hostip6.c",
        "curl/lib/hsts.c",
        "curl/lib/http.c",
        "curl/lib/http1.c",
        "curl/lib/http_chunks.c",
        "curl/lib/http_digest.c",
        "curl/lib/http_proxy.c",
        "curl/lib/idn.c",
        "curl/lib/if2ip.c",
        "curl/lib/llist.c",
        "curl/lib/macos.c",
        "curl/lib/md5.c",
        "curl/lib/mime.c",
        "curl/lib/mprintf.c",
        "curl/lib/multi.c",
        "curl/lib/multi_ev.c",
        "curl/lib/multi_ntfy.c",
        "curl/lib/parsedate.c",
        "curl/lib/peer.c",
        "curl/lib/progress.c",
        "curl/lib/protocol.c",
        "curl/lib/proxy.c",
        "curl/lib/rand.c",
        "curl/lib/ratelimit.c",
        "curl/lib/request.c",
        "curl/lib/select.c",
        "curl/lib/sendf.c",
        "curl/lib/setopt.c",
        "curl/lib/sha256.c",
        "curl/lib/slist.c",
        "curl/lib/socketpair.c",
        "curl/lib/socks.c",
        "curl/lib/splay.c",
        "curl/lib/strcase.c",
        "curl/lib/strequal.c",
        "curl/lib/strerror.c",
        "curl/lib/transfer.c",
        "curl/lib/uint-bset.c",
        "curl/lib/uint-hash.c",
        "curl/lib/uint-spbset.c",
        "curl/lib/uint-table.c",
        "curl/lib/url.c",
        "curl/lib/urlapi.c",
        "curl/lib/vauth/digest.c",
        "curl/lib/vauth/vauth.c",
        "curl/lib/version.c",
        "curl/lib/vtls/vtls.c",
        "curl/lib/vtls/vtls_config.c",
        "curl/lib/vtls/vtls_scache.c",
        "curl/lib/ws.c",
    ];

    if cfg!(feature = "http2") {
        sources.extend(["curl/lib/cf-h2-proxy.c", "curl/lib/http2.c"]);
        cfg.define("USE_NGHTTP2", None)
            .define("NGHTTP2_STATICLIB", None);

        println!("cargo:rustc-cfg=link_libnghttp2");
        if let Some(path) = env::var_os("DEP_NGHTTP2_ROOT") {
            let path = PathBuf::from(path);
            cfg.include(path.join("include"));
        }
    }

    println!("cargo:rustc-cfg=link_libz");
    if let Some(path) = env::var_os("DEP_Z_INCLUDE") {
        cfg.include(path);
    }

    sources.extend([
        "curl/lib/vtls/cipher_suite.c",
        "curl/lib/vtls/rustls.c",
        "curl/lib/vtls/x509asn1.c",
    ]);

    // Configure the mandatory rustls backend.
    cfg.define("USE_RUSTLS", None)
        .include(env::var_os("DEP_RUSTLS_FFI_INCLUDE").unwrap());

    // Configure platform-specific details.
    cfg.define("RECV_TYPE_ARG1", "int")
        .define("HAVE_ARPA_INET_H", None)
        .define("HAVE_ERRNO_H", None)
        .define("HAVE_FCNTL_H", None)
        .define("HAVE_NETDB_H", None)
        .define("HAVE_NETINET_IN_H", None)
        .define("HAVE_NETINET_TCP_H", None)
        .define("HAVE_POLL_H", None)
        .define("HAVE_FCNTL_O_NONBLOCK", None)
        .define("HAVE_SYS_SELECT_H", None)
        .define("HAVE_SYS_STAT_H", None)
        .define("HAVE_SYS_TIME_H", None)
        .define("HAVE_UNISTD_H", None)
        .define("HAVE_RECV", None)
        .define("HAVE_SELECT", None)
        .define("HAVE_SEND", None)
        .define("HAVE_SOCKET", None)
        .define("HAVE_STERRROR_R", None)
        .define("HAVE_SOCKETPAIR", None)
        .define("HAVE_STRUCT_TIMEVAL", None)
        .define("HAVE_SYS_UN_H", None)
        .define("USE_UNIX_SOCKETS", None)
        .define("RECV_TYPE_ARG2", "void*")
        .define("RECV_TYPE_ARG3", "size_t")
        .define("RECV_TYPE_ARG4", "int")
        .define("RECV_TYPE_RETV", "ssize_t")
        .define("SEND_QUAL_ARG2", "const")
        .define("SEND_TYPE_ARG1", "int")
        .define("SEND_TYPE_ARG2", "void*")
        .define("SEND_TYPE_ARG3", "size_t")
        .define("SEND_TYPE_ARG4", "int")
        .define("SEND_TYPE_RETV", "ssize_t")
        .define("SIZEOF_CURL_OFF_T", "8")
        .define("SIZEOF_INT", "4")
        .define("SIZEOF_SHORT", "2");

    if target.contains("-apple-") {
        cfg.define("__APPLE__", None)
            .define("HAVE_MACH_ABSOLUTE_TIME", None)
            // Rust's `std` provides the necessary symbols since:
            // https://github.com/rust-lang/rust/pull/138944
            .define("HAVE_BUILTIN_AVAILABLE", None);
        cfg.define("HAVE_CLOCK_GETTIME_MONOTONIC", None)
            .define("HAVE_GETTIMEOFDAY", None)
            // poll() on various versions of macOS are janky, so only use it
            // on non-macOS unix-likes. This matches the official default
            // build configuration as well.
            .define("HAVE_POLL_FINE", None);
    }

    cfg.define("SIZEOF_SSIZE_T", "8")
        .define("SIZEOF_SIZE_T", "8")
        .define("SIZEOF_LONG", "8");

    cfg.flag("-fvisibility=hidden");

    for source in &sources {
        let filename = Path::new(source).file_name().unwrap().to_str().unwrap();
        assert!(
            ![
                "curl_threads.c",
                "thrdpool.c",
                "thrdqueue.c",
                "asyn-thrdd.c"
            ]
            .contains(&filename),
            "thread source forbidden in single-thread libcurl: {}",
            source
        );
        cfg.file(source);
    }
    cfg.compile("curl");

    if target.contains("-apple-") {
        println!("cargo:rustc-link-lib=framework=Security");
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
        println!("cargo:rustc-link-lib=framework=CoreServices");
        println!("cargo:rustc-link-lib=framework=SystemConfiguration");
    }
}
